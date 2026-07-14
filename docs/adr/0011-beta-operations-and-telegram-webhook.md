# ADR 0011: beta operations и Telegram webhook boundary

Status: accepted

## Контекст

Закрытая beta требует воспроизводимого staging-контура, наблюдаемости,
восстановления PostgreSQL и blob store и production-safe Telegram transport.
Stage 6 уже отделил transport-neutral Telegram service от local long polling,
но публичного webhook boundary и проверяемого restore drill не было.

## Решение

- `/webhooks/telegram` создаётся только при валидном
  `LUMI_TELEGRAM_WEBHOOK_SECRET`; capability отсутствует, если route выключен.
- Secret header проверяется constant-time до buffering/parsing body. Payload,
  bot token, pairing token и raw message content не пишутся в logs/errors.
- Аутентифицированные permanent payload errors получают ACK без retry;
  transient storage/in-progress/timeout остаются retryable 5xx. Обработка
  использует тот же durable idempotent service, что local long polling.
- Long polling разрешён только в `local` mode. `staging` и `production`
  fail-closed требуют HTTPS origin, совпадающий auth audience и Secure cookies.
- Library continuation является одной owner-scoped SQL projection по текущей
  revision, без client N+1 и с индексированным progress ordering.
- Staging применяет migrations отдельным one-shot process до server startup;
  readiness проверяет PostgreSQL migration compatibility и полный bounded
  write/rename/read/delete sentinel blob backend.
- Raw PostgreSQL password и URL-encoded application `DATABASE_URL` доставляются
  раздельно; Compose не интерполирует raw secret внутрь URI.
- Logs — структурированный JSON с request id; alert contract хранится в repo.
- Backup согласован только при quiesced writes и содержит PostgreSQL dump,
  blob archive, manifest и checksums. Restore drill допускает только явно
  disposable database с suffix `_restore_drill`. Локальный drill без
  encrypted destination маркируется `drill_only` и не является beta evidence.

## Последствия

Webhook можно зарегистрировать у provider только после выдачи внешнего secret;
repository не утверждает, что это уже сделано. Filesystem blob volume остаётся
локальным reference backend; S3-compatible backend потребует отдельного ADR.
Частый readiness probe создаёт и удаляет маленький sentinel в выделенном
`.health`, зато проверяет требуемую atomic filesystem семантику.

## Отклонённые варианты

- Always-on webhook с default secret: fail-open и ложная capability.
- Production long polling: усложняет fencing и rollout.
- Backup только PostgreSQL или только blobs: не даёт согласованного material.
- Client scan progress: ограничивает библиотеку и создаёт N+1.

## Compatibility и fixtures

Additive `ContinueReadingEntry` и capability flag сохраняют v1 JSON clients.
Required gates: EPUB golden projection, committed Web/Telegram fixtures, SSRF
corpus, PostgreSQL isolation/Telegram suites, release performance budgets,
backup checksum и disposable restore drill.
