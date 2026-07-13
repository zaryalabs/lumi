# ADR 0004: PostgreSQL schema и минимальная sync-ready change model

Status: accepted

## Контекст

S1 должен заменить in-memory maps долговечным PostgreSQL state, но web schema
не должна стать отдельной моделью, которую придётся выбросить при появлении
native full-copy clients. Нужны одновременно:

- транзакционная запись web-команд;
- account/space isolation;
- immutable revisions и content-addressed blobs;
- optimistic concurrency для изменяемых объектов;
- tombstones вместо раннего hard delete;
- упорядоченный change feed для будущего bootstrap и incremental sync.

## Решение

PostgreSQL является system of record для relational state S1. SQLx migrations
создают следующие группы таблиц:

| Группа | Минимальные таблицы |
| --- | --- |
| Account | `accounts`, `account_profiles`, `auth_identities`, `auth_challenges`, `web_sessions`, `sync_devices` |
| Spaces/change feed | `sync_spaces`, `sync_space_members`, `sync_changes`, `sync_conflicts`, `idempotency_keys` |
| Library/import | `materials`, `document_revisions`, `normalized_packages`, `import_jobs`, `import_diagnostics` |
| Blobs | `blobs`, `blob_manifests`, `blob_manifest_entries` |
| Reader state | `annotations`, `reading_progress`, `reader_settings` |

Правила schema:

- доменные ids генерируются приложением как UUIDv7;
- timestamps хранятся как `timestamptz`, payloads/envelopes — как `jsonb`,
  checksums и public-key material — как `bytea`;
- каждая syncable content-запись содержит `space_id`; repository methods для
  content всегда принимают авторизованный `space_id` и не предоставляют
  unscoped read/write;
- account/security records (`accounts`, auth identities, sessions, devices)
  scoped по `user_id` и системной policy, а не по personal vault. Они могут
  пережить отключение или удаление cloud replica в будущем private mode;
- `document_revisions`, normalized package versions и blob contents immutable;
- mutable aggregates имеют `object_revision bigint`, который проверяется в
  `UPDATE ... WHERE object_revision = expected_revision`;
- delete создаёт state/tombstone с `deleted_at`; физическое удаление и blob GC
  выполняются отдельной retention policy;
- payload книги, XHTML и blob bytes не копируются в change log. Change payload
  содержит только syncable state или ссылки на immutable revision/blob.

`sync_changes` является append-only таблицей с формой:

```text
SyncChange {
  change_seq: bigint generated identity primary key
  change_id: uuid_v7 unique
  space_id: uuid
  object_type: text
  object_id: uuid
  object_revision: bigint
  base_revision: bigint?
  change_kind: create | update | delete | append | blob_ref | merge
  payload: jsonb
  device_id: uuid
  local_seq: bigint?
  hlc: text
  schema_version: text
  idempotency_key: text
  created_at: timestamptz
}
```

`change_seq` глобально монотонен, но durable cursor хранится отдельно для пары
`(device_id, space_id)`. Batched API может передавать map per-space cursors, но
не заменяет её одним account-wide cursor. Запрос каждого feed фильтруется по
конкретному разрешённому space; пропуски между его changes допустимы. Новый
доступ к существующему space начинается со snapshot/bootstrap и tail после его
cursor. Уникальны `(space_id, device_id, local_seq)` для native changes и
`(space_id, idempotency_key)` для повторяемой команды.

Application service в одной транзакции:

1. проверяет idempotency key и expected revision;
2. изменяет materialized domain table;
3. append-only записывает `sync_changes`;
4. сохраняет ответ команды для детерминированного retry;
5. commit-ит только полный набор изменений.

В S1 change log не compact-ится. Snapshot/bootstrap и compaction добавляются
позже поверх той же последовательности; `schema_version` и versioned reducers
являются обязательными до появления native клиента.

### Migration policy

- Файлы имеют вид `YYYYMMDDHHMMSS_description.sql`, являются forward-only и
  неизменяемы после попадания в shared branch.
- SQLx checksum history проверяется при startup/deploy. Production запускает
  migrations отдельным deploy step до переключения traffic, а не из каждого
  server instance.
- Миграция по возможности транзакционна. Для non-transactional PostgreSQL DDL
  это явно отражается в имени/runbook и проверяется отдельно.
- Breaking changes проходят expand → backfill → switch readers/writers →
  contract. Старый столбец удаляется только после совместимого deploy window и
  проверенного backup/restore path.
- Down migrations в production не поддерживаются. Rollback — предыдущий
  совместимый binary или forward repair migration; destructive repair требует
  backup snapshot.
- Каждая migration проверяется на пустой базе и на fixture snapshot предыдущей
  поддерживаемой версии.

## Последствия

- Web-команда и будущий sync change не могут разойтись после crash между двумя
  commits.
- Глобальная sequence упрощает PostgreSQL indexing, но выдаёт gaps и не является
  доменным временем. Revision и HLC помогают детерминированно упорядочить
  delivery; они не превращают content edits в LWW. Слабые preferences могут
  использовать LWW, а заметки и другие пользовательские документы проходят
  three-way merge или создают явный `sync_conflict` без потери версии.
- JSON payload ускоряет эволюцию envelope, но authoritative queryable поля
  остаются typed columns/domain tables.
- RLS можно добавить как defense-in-depth, однако она не заменяет обязательный
  account-scoped repository contract и isolation tests.

## Альтернативы

- Только нормализованные domain tables без change log: отклонено, потому что
  native bootstrap потребовал бы ad hoc diff protocol.
- Generic event sourcing как единственный source of truth: отклонено для S1 из-за
  сложности reducers, projections и migrations до проверки продукта.
- Пер-object sequence: отложено; глобальный identity даёт достаточный cursor с
  разрешёнными gaps.
- Автоматические down migrations: отклонено как ложная гарантия безопасного
  rollback для data migrations.

## Compatibility

- Stage 1 реализует migrations и repository integration tests на PostgreSQL.
- Domain schema markers не равны SQL migration versions; оба значения хранятся
  и тестируются отдельно.
- Sync fixtures должны проверять duplicate idempotency key, stale revision,
  tombstone и порядок changes после cursor.
- Решение уточняет `SYNC-001`, `SYNC-002`, `API-002`, `CORE-002`, `CORE-009` и
  `ACC-003` без изменения reader contracts.
