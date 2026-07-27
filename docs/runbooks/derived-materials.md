# Производные `.lum`-материалы

Status: `accepted`

Этот runbook описывает выпуск `0.2.0/E4`: сокращение исходного материала,
проверку generated package, атомарную публикацию в библиотеку и восстановление
после прерывания.

## Пользовательский flow

1. В сведениях об обычном готовом материале выбрать «Создать сокращённый
   материал».
2. Выбрать профиль `Сбалансированное` или `Краткое` и запустить задачу.
3. Внутренний OpenRouter worker либо claimed MCP executor формирует
   `abridgement-artifact.v1` с Markdown-главами и citations.
4. Сервер собирает и проверяет `.lum`, публикует отдельный Material и показывает
   его в библиотеке с меткой «Производный материал».
5. Материал открывается обычным Reader. В сведениях доступны immutable revision
   оригинала, точные переходы к источникам и download исходного `.lum`.

Повторное открытие или recovery не создаёт дубль: один artifact может
соответствовать только одному derived material. Новая revision оригинала не
меняет существующее сокращение, а включает состояние «оригинал обновлён».

## Граница доверия и валидация

AI executor возвращает только bounded JSON:

- `schema_version = abridgement-artifact.v1`;
- `profile = brief | balanced`;
- не более 128 глав;
- суммарный Markdown не более 2 MiB;
- citations каждой главы входят в общий набор и вместе точно его покрывают.

Готовый ZIP от provider или MCP не принимается. Lumi назначает безопасные
chapter paths, пишет `lum.toml`, Markdown chapters и
`META-INF/lumi/provenance.json`, считает checksums и повторно вызывает обычный
`.lum` importer. Portable provenance имеет версию
`lumi.generated-provenance.v1` и связывает каждую главу с source citations.

## Публикация и recovery

Task completion атомарно завершает run/job/task и создаёт candidate artifact.
Публикация material projection выполняется следующей транзакцией:

```text
validated artifact
  -> source/resources/normalized blobs
  -> Material + DocumentRevision + normalized package
  -> succeeded import job + material_derivations + sync change
  -> active artifact
```

Blobs content-addressed и сами по себе не видны пользователю. Все database
строки derived material становятся видимыми только после commit. При crash
после task completion startup recovery сканирует валидные candidate
abridgement artifacts и повторяет идемпотентную публикацию. Невалидный
исторический candidate не публикуется; ошибка storage/readiness не маскируется.

## HTTP и MCP

Web создаёт задачу через:

```text
POST /api/v1/materials/{material_id}/abridgement-tasks
```

MCP tool `create_abridgement_task` вызывает тот же application service.
`claim_ai_task`/`complete_ai_task` используют общий fence; abridgement result
проходит тот же preflight и publisher, что internal worker. Account scope,
CSRF для Web, bearer verifier для MCP, request/body limits и idempotency
остаются общими границами.

## Диагностика

- `invalid_abridgement_package` — payload, citations, assembly или повторный
  import не прошли validation;
- `source_revision_unavailable` — immutable source revision недоступна;
- `source_changed` в Web — active revision оригинала уже отличается;
- storage/readiness failure во время recovery — проверить PostgreSQL и blob
  store, затем перезапустить server; повторная публикация безопасна.

В логах допустимы task/artifact/material IDs, stage и redacted error code.
Текст исходника, context pack, provider body и содержимое глав не логируются.

## Проверка выпуска

```sh
make l
make pg-t
make compatibility
make security
make performance
make web-e2e
make c
```

PostgreSQL integration проверяет идемпотентную публикацию, точный provenance,
неизменность source revision и чтение derived revision. Playwright проходит
flow «создать → Queue → библиотека → provenance → Reader». Export проверяется
через обычный source download, а полученный `.lum` — тем же importer corpus.
