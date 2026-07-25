# ADR 0019: schema AI task, run и artifact

Status: accepted

## Контекст

Фоновые саммари и сокращение материалов должны одинаково исполняться
внутренним provider worker и внешним MCP-агентом. При этом пользовательская
задача, техническая попытка, claim и опубликованный результат имеют разные
жизненные циклы. Смешивание их в одной строке не позволяет безопасно повторять
задачу, менять исполнителя, защищаться от stale completion и сохранять ручные
правки.

Решение меняет PostgreSQL schema, sync projection и публичные HTTP/MCP
контракты, поэтому принимается до migrations.

## Решение

- `ai_tasks` хранит account-scoped пользовательскую операцию: расширяемые
  `kind` и `result_kind`, revision-bound source scope, canonical parameters,
  prompt/output schema versions, priority, status, progress и cancellation.
- Пользовательские статусы: `queued`, `running`, `needs_input`, `succeeded`,
  `failed`, `cancelled`. `claimed` и истечение lease являются техническим
  состоянием attempt, а не отдельным стабильным состоянием task.
- `ai_runs` хранит каждую попытку: executor kind/id, provider/model, status,
  timestamps, usage/cost metadata, redacted error и immutable
  `context_pack_id`. Повтор создает новый run и не переписывает diagnostics
  предыдущего.
- Claim содержит случайный `claim_id`, монотонный `fence`, `lease_expires_at`
  и revision task на момент claim. Heartbeat, progress, complete, fail и release
  принимаются только при совпадении active run, `claim_id` и `fence`.
  Истекший claim не может публиковать результат.
- `ai_context_packs` хранит versioned manifest разрешенного контекста, hashes,
  source refs, permission snapshot и фактически отправленные bounded fragments.
  Pack immutable и принадлежит одному account/task/run.
- `ai_artifacts` хранит только типизированный payload, прошедший проверку
  `artifact_kind + schema_version`. Частичный provider output остается
  execution data и не публикуется как artifact.
- Общий artifact lifecycle: `candidate`, `active`, `rejected`, `superseded`.
  Для summary отдельный slot с уникальностью
  `owner + source_revision + scope_kind + scope_ref + form` указывает на один
  active artifact и, максимум, один новый candidate.
- Ручная правка создает новую immutable artifact revision с
  `authored_by = user`. Новая generation при наличии пользовательской правки
  становится candidate; автоматическая замена active запрещена.
- Active-task dedupe использует versioned `dedupe_key` от owner, kind,
  revision-bound scope, canonical parameters, prompt и output schema versions.
  Partial unique index действует для `queued`, `running` и `needs_input`.
- Create и complete имеют отдельные idempotency keys. Повтор complete с тем же
  key и payload hash возвращает прежний результат; другой payload или stale
  fence дает conflict.
- Task и artifact входят в account sync как пользовательские данные.
  Run diagnostics, leases и provider transport events не синхронизируются.
  Context pack синхронизирует manifest/source refs, но не secret/provider
  headers.

## Последствия

- Внутренний provider и MCP agent используют одну transactional publication
  command.
- Retry, recovery и смена исполнителя не создают duplicate artifact.
- Enum-поля хранятся как проверяемые строки с capability validation, чтобы
  добавлять task/artifact kinds без breaking migration.
- Chat conversation/generation schema остается отдельной: chat turn не создает
  `AiTask`.

## Стратегия migrations

AI tables добавляются forward-only migrations без изменения существующих
material/import rows. Owner ids и source revision ids получают foreign keys и
owner-prefixed indexes. Publication выполняется одной транзакцией: проверка
claim/fence, вставка artifact revision, обновление slot/result ref, завершение
run и task.

Удаление source material следует существующей lifecycle policy: task/artifact
сохраняют tombstoned provenance, но не дают прочитать недоступный source
context. Новая source revision не меняет старый результат автоматически.

## Альтернативы

- Одна таблица task/run/result: отклонено, потому что retry и provenance
  перезаписывают друг друга.
- Artifact как произвольный JSON без versioned validator: отклонено; такой
  результат нельзя безопасно публиковать или принимать от MCP.
- Перезаписывать active summary при regeneration: отклонено из-за потери
  пользовательских правок.
- Закреплять queued task за provider: отклонено; это ломает конкуренцию
  internal worker и MCP agent.

## Совместимость и проверки

- Schema marker первого AI-среза: `ai.contract.v1`; конкретный domain marker
  повышается на Contract Freeze 1 вместе с core types.
- Обязательны PostgreSQL race fixtures для create/claim/heartbeat/complete,
  stale fence, duplicate completion, active summary uniqueness, manual edit и
  transactional publication.
- JSON snapshots находятся в `tests/fixtures/ai/contracts/v1/` и меняются
  только versioned contract change.
