# Web и Telegram baseline: локальный запуск

Status: active

## Web URL

```sh
make db-up
make db-migrate
make server-r
make web-r
```

«Добавить материал → Web-ссылка» вызывает `POST /api/v1/imports/url` с JSON
`{"url":"https://example.org/article"}`, session, CSRF и `Idempotency-Key`.
Fetcher принимает public HTTP(S), максимум четыре redirects и 2 MiB response,
повторно проверяет DNS каждого hop, pin-ит соединение, не использует proxy,
cookies, scripts или subresources. Snapshot сохраняется до extraction и
повторно используется при retry.

Fixture provider работает только при явном local env и только для
`fixtures.lumi.test`:

```sh
LUMI_WEB_FIXTURE_ROOT=tests/fixtures/web make server-r
```

URL `https://fixtures.lumi.test/article` тогда не обращается к сети.

## Telegram long polling

API server и runner должны получить один уникальный scope конкретного bot:

```sh
export LUMI_TELEGRAM_BOT_SCOPE=lumi-local-my-bot
export LUMI_TELEGRAM_BOT_USERNAME=my_lumi_bot
export LUMI_TELEGRAM_BOT_TOKEN='секрет из BotFather'
make server-r
make telegram-r
```

Не запускайте два runner с одним scope: PostgreSQL advisory lock отклонит
второй. Не переиспользуйте scope для другого bot. Token нельзя передавать в
аргументах, URL приложения, logs или fixtures. Long polling — только local
transport. Опциональный production-safe webhook/secret boundary теперь описан
в [`beta-staging.md`](beta-staging.md): route отсутствует без runtime secret, а
факт внешней регистрации подтверждает operator, не repository.

В UI plaintext pairing token показывается один раз, исчезает после connection
или expiry и не кэшируется response. `/start <token>`, `/help`, `/unlink`,
direct/forwarded text и одна public web URL поддерживаются. Group/media/batches
не импортируются. Text и ограниченная forward attribution сохраняются как
личный cloud content. Unlink запрещает новые imports, но не удаляет материалы.

## Limits и проверка

- `429`: уже 16 queued/running imports аккаунта; дождитесь завершения.
- Одновременно работает до 8 normalizers на process.
- Source download: attachment, `private, no-store`, `nosniff`.
- API server и runner используют один claim/lease-safe recovery: активный lease
  не перехватывается, а queued/expired job получает ровно одного worker owner.
- Duplicate Telegram update возвращает durable outcome без второго material.

```sh
make c
make web-e2e
```
