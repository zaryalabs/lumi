# ADR 0020: общий Job Runtime без одномоментного переписывания import

Status: accepted

## Контекст

Durable import уже реализует atomic claim, lease, heartbeat, retry,
cancellation, recovery и diagnostics внутри `import_jobs`. AI, indexing,
export и repair требуют тех же гарантий. Копирование state machine создаст
несовместимые recovery semantics, но одномоментный перенос рабочего import
pipeline в новую schema слишком рискован для `0.2.0`.

## Решение

- `JobRuntime` является общим application/runtime contract с операциями
  `enqueue`, `claim`, `heartbeat`, `set_progress`, `request_cancel`,
  `complete`, `fail`, `release` и `recover_expired`.
- Общая state machine использует `queued`, `running`, `succeeded`, `failed`,
  `cancelled`; reservation/preparation stages остаются feature-specific.
  Claim всегда содержит случайный id, монотонный fence и конечный lease.
- Runtime не знает payload конкретной подсистемы. `JobDescriptor` содержит
  owner, kind, payload ref, limits/retry policy и redacted diagnostics ref.
- В `0.2.0` AI и новые background kinds используют generic `jobs` persistence.
  `AiTask` является пользовательским aggregate, а `Job` — технической
  попыткой исполнения; один task может породить несколько job attempts.
- Существующий import остается authoritative в `import_jobs`, но переводится
  на тот же `JobRuntime` algorithm через `ImportJobRepository` adapter.
  Сначала из `imports.rs` выделяются transition/lease/recovery services и
  contract tests; таблица и API import не меняются одновременно.
- Нельзя вести две authoritative lifecycle-записи для одного выполнения.
  Shadow mirroring допустим только как read-only metric во время будущей
  migration и не участвует в claim/recovery.
- Worker выполняет bounded admission до claim, регулярно heartbeat-ит lease,
  проверяет cancellation между дорогими стадиями и завершает работу только
  fenced update. Process restart запускает idempotent expired-lease recovery.
- Feature-specific progress представляет versioned stage + bounded числовое
  значение. Runtime не логирует payload или content bodies.

## Последствия

- AI может использовать проверенный execution contract без копирования import
  SQL и без рискованной миграции существующих jobs.
- На переходном этапе существуют два persistence adapters, но одна state
  machine и один набор concurrency invariants.
- Полное перемещение import payload/lifecycle в generic tables остается
  отдельной migration после parity tests; оно не блокирует `0.2.0`.

## Альтернативы

- Немедленно перенести все import rows в generic `jobs`: отклонено из-за
  большого blast radius для действующего S1 pipeline.
- Оставить отдельный `ai_jobs` runtime: отклонено как долгосрочное дублирование
  claim/recovery semantics.
- In-memory queue поверх task table: отклонено; задачи должны переживать restart
  и конкурирующих workers.

## Совместимость и проверки

- Существующие import job ids, API statuses и recovery behavior сохраняются.
- Один общий conformance suite запускается для generic и import adapters:
  atomic claim, stale lease/fence, heartbeat, cancellation race, retry budget,
  restart recovery и redacted diagnostics.
- Import adapter принимается только после прохождения текущих PostgreSQL,
  compatibility и security suites без ослабления assertions.
