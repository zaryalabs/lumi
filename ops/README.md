# Production operations Lumi

Эта директория задаёт Git-owned production contract, который устанавливается в
`/opt/apps/lumi`. Runtime secrets, активный release pointer, состояние и backup
остаются на сервере и никогда не копируются обратно в Git.

## Layout installation

```text
/opt/apps/lumi/
  Makefile
  README.md
  compose.yaml
  .env
  .env.images -> builds/releases/<release-id>.env.images
  builds/releases/
  scripts/
  volumes/postgres-data/
  volumes/blobs/
  backups/
```

`ops/Makefile`, `ops/README.md`, `ops/compose.yaml` и repository-side
backup/restore scripts устанавливаются как отдельно reviewed изменение
operational contract. Обычный релиз устанавливает только release manifest и
вызывает этот Makefile.

## Bootstrap

Bootstrap меняет production state и выполняется оператором после review:

1. Создать `/opt/apps/lumi/{builds/releases,scripts,volumes/postgres-data,volumes/blobs,backups}`.
2. Установить reviewed-файлы из `ops/`, включая
   `ops/validate-release-manifest.sh` как
   `/opt/apps/lumi/scripts/validate-release-manifest.sh`, а также
   `scripts/backup.sh` и `scripts/restore-drill.sh`. Root-owned
   `ops/lumi-ci-root` устанавливается как
   `/usr/local/sbin/lumi-ci-root-deploy` с mode `0755`, а
   `ops/sudoers.example` — как `/etc/sudoers.d/lumi-ci` с mode `0440` после
   проверки `visudo -cf`.
3. Создать `.env` из `.env.example`, заменить все secrets и закрепить PostgreSQL
   и BusyBox images по digest. Установить mode `0600`.
4. Убедиться, что external Docker network `platform` существует.
5. Добавить узкое sudoers-правило для `runner`, разрешающее только
   `/usr/local/sbin/lumi-ci-root-deploy`, и проверить наличие `jq`. Workflow
   передаёт root wrapper только job-scoped GHCR auth; постоянный root Docker
   login не нужен.
6. Установить release manifest, активировать его и выполнить `make deploy`.

Приложение использует `https://lumi.zrya.io`. Основной Web/API router применяет
только общие security headers через `platform-headers@file`: внешний Basic Auth
не используется, а пользовательскую аутентификацию выполняет Lumi. Точный
Telegram webhook path `/api/v1/webhooks/telegram` защищён проверкой Lumi header
`X-Telegram-Bot-Api-Secret-Token`.

## Releases

```sh
make releases
make activate RELEASE=<release-id>
make deploy
make rollback RELEASE=<previous-release-id>
```

`.env.images` является только active symlink. Release manifests содержат полный
Git SHA и digest-pinned server/web images. Production никогда не использует
`latest`.

Forward migrations выполняются до замены приложения. Изменения schema должны
быть совместимы как минимум с предыдущим release приложения; иначе rollback
требует согласованного восстановления PostgreSQL и blobs.

## Operations

```sh
make help
make status
make logs S=server
make smoke
make backup LUMI_BACKUP_DESTINATION_ENCRYPTED=1
make restore-verify BACKUP=<backup-id>
```

`make backup` намеренно останавливает server для quiesce writes, создаёт общий
PostgreSQL + blob backup и снова запускает server даже при ошибке backup.
Оператор передаёт `LUMI_BACKUP_DESTINATION_ENCRYPTED=1` только после проверки,
что `backups/` находится на encrypted destination или покрыт синхронной
encrypted off-site копией. Перед допуском beta-пользователей выполняется
disposable restore drill.

## Logs

Lumi пишет структурированные JSON logs в stdout/stderr. Общий Promtail
автоматически обнаруживает Docker containers, поэтому отдельный product log
shipper не нужен. Начальные LogQL queries:

```logql
{container="lumi-server"}
{container="lumi-server"} | json | level="ERROR"
{container="lumi-server"} |= "request_id"
{container=~"lumi-(server|web|postgres|migrate)"}
```

Alert rules, dashboards, OTLP metrics и Prometheus readiness probes не входят в
первый release.
