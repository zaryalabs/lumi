# Production deploy Lumi

Status: repository-side executable contract

Этот runbook описывает main-only self-hosted CI/CD и установку Lumi на target
`zarya-main`. Он не утверждает, что GitHub settings, DNS, server secrets или
`/opt/apps/lumi` уже настроены.

## Trust boundary

Lumi является публичным репозиторием, но workflows не запускаются на событиях
pull request. Команда проверяет PR вручную, и только accepted merge в `main`
попадает на self-hosted runner.

```text
reviewed merge в main
-> self-hosted prepare
-> self-hosted build
-> push digest-pinned images в GHCR
-> manual workflow_dispatch
-> deploy через /opt/apps/lumi/Makefile
```

В `.github/workflows/` намеренно отсутствуют `pull_request` и
`pull_request_target`. GitHub должен запрещать direct pushes в `main` и требовать
review. До merge автор или reviewer запускает `make prepare` локально и фиксирует
результат в PR.

## GitHub setup

До первого merge с workflows:

1. Защитить `main` от direct push.
2. Требовать PR review от Zarya maintainers.
3. Создать GitHub Environment `production-main` и ограничить deploy trusted
   maintainers, когда доступна environment protection.
4. Разрешить `GITHUB_TOKEN` публиковать и читать packages репозитория.
5. Убедиться, что runner имеет labels `self-hosted`, `zarya-main`, `geo-eu`,
   `ci`, `deploy`, членство в группе `docker` и узкий non-interactive sudo
   contract только для Lumi deploy wrapper.

`main.yml` автоматически выполняет `make prepare`, `make build` и `make push`.
`deploy.yml` запускается только вручную из `main`, проверяет полный SHA,
принадлежность commit ветке `main` и наличие успешного main workflow run. Затем
он скачивает release manifest именно из artifact этого run; digest не
перевычисляется по потенциально изменяемому registry tag.

## Server bootstrap

Bootstrap является отдельным operator action и требует явного разрешения на
production changes. Из reviewed checkout установить:

```text
ops/Makefile             -> /opt/apps/lumi/Makefile
ops/README.md            -> /opt/apps/lumi/README.md
ops/compose.yaml         -> /opt/apps/lumi/compose.yaml
scripts/backup.sh        -> /opt/apps/lumi/scripts/backup.sh
scripts/restore-drill.sh -> /opt/apps/lumi/scripts/restore-drill.sh
ops/validate-release-manifest.sh -> /opt/apps/lumi/scripts/validate-release-manifest.sh
ops/lumi-ci-root         -> /usr/local/sbin/lumi-ci-root-deploy
ops/sudoers.example      -> /etc/sudoers.d/lumi-ci
```

Создать server-owned `.env` по `ops/.env.example`, не копируя secrets в Git.
Production third-party images должны быть digest-pinned. Проверить сеть
`platform`, DNS `lumi.zrya.io` и возможность Traefik получить TLS certificate.
Deploy workflow создаёт job-scoped GHCR login. Root wrapper проверяет его
строгую JSON-форму, копирует только opaque `ghcr.io` auth в root-owned runtime
directory и удаляет после deploy; долгоживущий root PAT не требуется.
`jq` должен быть установлен на target. Sudoers template проверяется
`visudo -cf ops/sudoers.example` до установки и содержит точное правило для
`runner`:

```sudoers
runner ALL=(root) NOPASSWD: /usr/local/sbin/lumi-ci-root-deploy
```

После успешного main release запустить `Deploy Lumi production`, передав полный
commit SHA. Workflow переносит проверенный CI manifest в
`/opt/apps/lumi/builds/releases/`, активирует его и вызывает `make deploy`.
Manifest проверяется как данные со строгим набором полей и никогда не
исполняется через shell `source`/`.`.

## Acceptance

Первый deploy принимается после следующих проверок:

- `make status` показывает healthy `postgres`, `server` и `web`;
- internal `/api/v1/ready` отвечает успешно;
- external `https://lumi.zrya.io` отвечает `200` или ожидаемым Basic Auth `401`;
- registration/login, EPUB import, reader, progress и annotations проходят;
- restart `server` не теряет PostgreSQL или blob state;
- logs `lumi-server`, `lumi-web` и `lumi-postgres` видны в Loki/Grafana;
- Telegram webhook регистрируется только после основного smoke;
- encrypted backup и disposable restore drill подтверждены до допуска beta users.

Alert rules, dashboards, OTLP metrics и Prometheus probes выполняются отдельной
задачей.

## Rollback

```sh
cd /opt/apps/lumi
make releases
make rollback RELEASE=<previous-release-id>
```

Image rollback безопасен только при backward-compatible schema. Для
несовместимой migration требуется quiesce writes и согласованное восстановление
PostgreSQL и blobs.
