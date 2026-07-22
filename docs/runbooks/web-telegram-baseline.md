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

## Telegram-бот

Запустите обычный стек и войдите в Lumi. Откройте «Настройки → Telegram-бот»,
введите token из BotFather и дождитесь статуса «Работает». Основной
`lumi-server` сам проверяет token через `getMe` и запускает встроенный long
polling listener на `teloxide-core`; отдельный процесс и env для Telegram не
нужны.

Настройка глобальна для экземпляра Lumi. На текущем прототипном этапе её может
изменить любой авторизованный пользователь. Токен не возвращается в browser,
PostgreSQL содержит только шифротекст, а master key лежит в persistent secret
root. При замене токена тем же bot id пользовательские привязки сохраняются;
другой bot id требует нового pairing.

PostgreSQL advisory lock защищает от случайного запуска двух listeners для
одного bot id. Webhook сохранён только как будущее направление в
[ADR 0012](../adr/0012-embedded-telegram-bot-settings.md).

В UI plaintext pairing token показывается один раз, исчезает после connection
или expiry и не кэшируется response. `/start <token>`, `/help`, `/unlink`,
direct/forwarded text и одна public web URL поддерживаются. Group/media/batches
не импортируются. Text и ограниченная forward attribution сохраняются как
личный cloud content. Unlink запрещает новые imports, но не удаляет материалы.

## Limits и проверка

- `429`: уже 16 queued/running imports аккаунта; дождитесь завершения.
- Одновременно работает до 8 normalizers на process.
- Source download: attachment, `private, no-store`, `nosniff`.
- API server и встроенный listener используют один claim/lease-safe recovery: активный lease
  не перехватывается, а queued/expired job получает ровно одного worker owner.
- Duplicate Telegram update возвращает durable outcome без второго material.

```sh
make c
```

Живой token и доставку сообщения проверяют вручную локально или на сервере;
browser E2E для этого provider-сценария не требуется.
