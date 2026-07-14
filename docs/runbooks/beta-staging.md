# Закрытая beta: staging, monitoring, backup и restore

Status: executable repository baseline

Production installation и main-only self-hosted CI/CD вынесены в
[production-deploy.md](production-deploy.md). Этот staging runbook остаётся
локально воспроизводимым beta gate и не заменяет production operations contract.

## Границы доказательства

Этот runbook проверяет локально воспроизводимую staging topology. Он не
утверждает, что внешний deployment, DNS/TLS, Telegram webhook registration,
secret delivery, scheduled backup или restore managed infrastructure уже
выполнены. Эти действия требуют отдельного operator acceptance.

## Конфигурация и запуск

Скопируйте `deployments/staging.env.example` во вне-repository secret source и
замените значения. Не коммитьте реальные пароли/token/secret.
`LUMI_POSTGRES_PASSWORD` передаётся PostgreSQL как raw secret, а отдельный
`LUMI_DATABASE_URL` должен содержать тот же пароль в URL-encoded форме; URI не
собирается интерполяцией, поэтому `@`, `:`, `/`, `%` и пробелы в secret не
ломают подключение. Предпочтительно выдавать оба значения через secret manager.

```sh
make staging-config
docker compose --env-file /secure/path/lumi-staging.env \
  -f deployments/compose.staging.yaml up -d --build --wait
```

`migrate` завершается до старта `server`. `/api/v1/health` — liveness без
dependencies; `/api/v1/ready` проверяет PostgreSQL, число успешных migrations и
bounded blob sentinel write/atomic rename/read/delete за две секунды. Server
работает non-root с read-only root filesystem и writable named blob volume.
Reference Compose публикует только API на loopback. Web artifact и TLS
terminator находятся во внешнем deployment boundary: operator должен собрать
Dioxus Web с тем же HTTPS API origin, обслужить его через TLS reverse proxy и
проверить CORS/origin до открытия beta. `make staging-smoke` собирает server
image, запускает изолированный Compose project и проверяет health/readiness.

Telegram webhook включается только непустым secret длиной 32–256 visible ASCII.
После operator registration Telegram должен отправлять JSON с
`X-Telegram-Bot-Api-Secret-Token`. `telegram-webhook` появляется в capabilities
только при включённом route. Long polling в staging/production завершится с
ошибкой до подключения к provider.

## Logs и alerts

Server/runner пишут JSON events. Request middleware создаёт и возвращает
`x-request-id`; events не содержат request/Telegram body или runtime secrets.
Минимальные vendor-neutral alerts заданы в `deployments/alerts.yaml`: readiness,
error rate, import failures и stale backup. Перед beta operator должен привязать
эти signals к конкретному log/metrics backend и проверить тестовый alert.

## Backup

Сначала остановите mutations/workers или включите maintenance boundary, затем:

```sh
PGSERVICE=lumi-staging LUMI_BACKUP_WRITES_QUIESCED=1 \
LUMI_BACKUP_DESTINATION_ENCRYPTED=1 LUMI_BLOB_ROOT="$LUMI_BLOB_ROOT" \
./scripts/backup.sh /secure/backups/lumi
```

Manifest `lumi.backup.v1` связывает custom PostgreSQL dump и blob archive;
`SHA256SUMS` проверяет оба artifacts. Без подтверждения quiesced writes script
fail-closed. Расписание, retention, encryption и off-site copy настраивает
operator и подтверждает отдельно.

Только для disposable локальной проверки механики допускается
`LUMI_BACKUP_DRILL_MODE=1` без заявления о шифровании destination. Такой
manifest явно содержит `destination_encrypted: false` и `drill_only: true` и
не принимается как `RESTORE_ATTESTATION` закрытой beta.

## Disposable restore drill

```sh
PGSERVICE=lumi-staging RESTORE_DATABASE_NAME=lumi_restore_drill \
./scripts/restore-drill.sh /secure/backups/lumi/<timestamp>
```

Script отклоняет database без suffix `_restore_drill`, проверяет checksums,
восстанавливает dump и blobs во временный каталог, проверяет ключевые relations
и всегда удаляет disposable database. Для принятия beta сохраните timestamp,
manifest checksum и вывод `restore drill passed` во внешнем operator log.

## Gates

```sh
make beta-local
make beta
```

`beta-local` поднимает PostgreSQL, применяет migrations и запускает обязательные
PG, compatibility, security, release performance, validator contract,
`make c` и browser E2E suites. Он доказывает repository mechanics, но не
принимает закрытую beta.

`beta` дополнительно требует `RESTORE_ATTESTATION` — JSON
`lumi.restore-attestation.v1`, который ссылается на backup manifest,
`SHA256SUMS` и сохранённый restore output. Validator повторно считает hashes,
требует `destination_encrypted: true`, `drill_only: false`, timezone timestamp,
operator/environment/disposable database identity, точное совпадение row counts
и blob records и строку `restore drill passed`. Произвольный или несвязанный
файл отклоняется. Без этого внешнего evidence закрытая beta намеренно не может
считаться принятой.
