# AI persistence и общий Job Runtime

Статус: исполняемый foundation `0.2.0/A1`

Этот runbook описывает уже реализованную базовую инфраструктуру AI. Он не
означает, что пользовательские AI capabilities включены: provider routes,
explicit context resolver, worker, Web queue и MCP transport поставляются
последующими этапами.

## Migration

Forward-only migrations
`20260726120000_ai_persistence_job_runtime.sql` и
`20260726130000_ai_persistence_invariants.sql` добавляют:

- `secret_envelopes`, provider preferences и credential references;
- общий `jobs`/`job_diagnostics` runtime;
- `ai_tasks`, `ai_runs`, `ai_context_packs`, `ai_artifacts` и
  `ai_task_completions`;
- owner-prefixed, source и recovery indexes;
- partial uniqueness для active task dedupe, active provider credential и
  summary candidate/active slot;
- composite ownership FK, строгую форму lifecycle rows и durable mapping
  нескольких create idempotency keys на одну active task;
- монотонный `worker_fence` для adapter существующего `import_jobs`.

Existing `import_jobs` не копируются и не зеркалируются в `jobs`.
`ImportJobRepository` применяет к ним общий claim/lease/fence/recovery
contract, а import-specific publication и material projection остаются одной
транзакцией import service. Corrective migration backfill-ит только
authoritative owner membership для ранее созданных owner jobs; cross-owner
связи блокируются composite FK.

Применение:

```sh
make db-up
make db-migrate
```

Migration только additive. Rollback приложения не удаляет новые таблицы и не
требует rollback schema.

## SecretStore

`SecretStore` хранит в PostgreSQL только AES-256-GCM envelope:

- ciphertext и 96-bit nonce;
- account/purpose scope;
- key version;
- keyed HMAC fingerprint;
- optimistic revision.

AAD связывает ciphertext с instance id, account id, secret id, purpose и key
version. Перенос ciphertext в другую строку, purpose или account не проходит
аутентификацию.

В `LUMI_SECRET_ROOT` находятся:

```text
secret-store.instance
secret-store.active
secret-store-v<version>.key
```

Это operator-controlled material с правами `0700` для каталога и `0600` для
файлов на Unix. Он не хранится в PostgreSQL и должен резервироваться отдельно
от database dump. Restore требует согласованную копию PostgreSQL и этого key
ring.

Активация новой версии сначала создаёт новый key file и active marker.
Existing envelopes остаются доступны через decrypt-only старые versions и
перешифровываются optimistic lazy rewrap при чтении либо batch
`SecretStore::rewrap_all`. Старый key нельзя удалять до database scan,
подтверждающего отсутствие соответствующей `key_version`, и отдельного restore
drill.

Telegram при первом чтении старой singleton envelope с
`telegram-token.key` выполняет совместимую lazy migration в `SecretStore`.
Новые сохранения сразу используют общий envelope; legacy key не создаётся
заново. Lazy migration, concurrent replace и delete сериализуются PostgreSQL
advisory lock и меняют settings row/envelope в одной транзакции, поэтому
orphan envelope или mixed legacy/new row не являются допустимым исходом.

## Job Runtime

Общий `JobRuntime` предоставляет:

```text
enqueue -> claim -> heartbeat/set_progress -> complete
                                      \----> fail/release
queued/running ----------------------------> cancel
expired lease ----------------------------> queued | failed | cancelled
```

Claim содержит случайный id, монотонный fence и конечный lease. Все mutation
операции проверяют claim id, fence, owner, running state и неистёкший lease.
Recovery requeue-ит только retryable work с оставшимся attempt budget;
cancelled/exhausted work становится terminal. Новый attempt сбрасывает
progress/error текущего execution row, а повтор successful completion с тем же
claim/fence возвращает уже сохранённый terminal outcome.

Adapters:

- `PgJobRepository` — новые AI и будущие background kinds в `jobs`;
- `ImportJobRepository` — существующая authoritative `import_jobs`.

Payload refs и content bodies runtime не логирует. Failure code/message
ограничены и сохраняются без raw feature payload.

## AI repository

`PgAiRepository` проверяет owner scope на каждом чтении и mutation:

- create одновременно создаёт `AiTask` и generic `Job`;
- request idempotency и active dedupe являются отдельными invariants;
- context pack immutable и должен совпадать с exact task source revision;
- один context pack используется только одним run; retry требует новый pack;
- claim одной транзакцией переводит task/job в running и создаёт `AiRun`;
- complete сначала проверяет output schema и выданные citation ids;
- artifact insert, completion replay record, run/job/task success коммитятся
  одной транзакцией;
- expired claim reconciles в released/queued либо terminal state, старый claim
  больше не может опубликовать artifact.

## Проверки

Обязательный PostgreSQL gate:

```sh
make pg-t
```

Он включает competing claims, stale fence/lease, retry exhaustion, restart
recovery, owner isolation, active uniqueness, idempotent create/complete,
transactional artifact publication, keyed secret rotation/tamper checks и
conformance import adapter.

Security gate:

```sh
make security
```

Дополнительно перед handoff выполняется `make c`. После A1
`AiCapabilityReadiness::a1_foundation()` подтверждает готовность
persistence/job/secret infrastructure, но не публикует ни одного `ai-*` или
`mcp-*` product feature id.
