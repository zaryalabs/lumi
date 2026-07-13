# Persistent account: локальный запуск и проверка

Status: active

Этот runbook описывает Этап 1 `S1 Web Reader`: PostgreSQL migrations,
seed-derived Ed25519 auth, web sessions, devices и account isolation.

## Локальный запуск

Запустить PostgreSQL и применить forward-only migrations:

```sh
make db-up
make db-migrate
```

Если порт `5432` занят, один и тот же override нужен обеим командам и серверу:

```sh
make db-up LUMI_POSTGRES_PORT=55432
make db-migrate LUMI_POSTGRES_PORT=55432
make server-r LUMI_POSTGRES_PORT=55432
```

После миграции запустить API и web в отдельных терминалах:

```sh
make server-r
make web-r
```

`dx` проксирует `/api/*` на Axum, поэтому browser использует same-origin
requests и cookies. На локальном HTTP сессия называется `lumi_session`. В
hosted HTTPS окружении нужно задать `LUMI_SECURE_COOKIE=true`; тогда сервер
выдаёт `__Host-lumi_session` с `Secure`, `HttpOnly`, `SameSite=Lax` и `Path=/`.

## Auth flow

Регистрация в browser создаёт 24-word BIP39 phrase из 256 бит entropy. Browser
через HKDF-SHA-256 независимо выводит account lookup key и Ed25519 signing key.
На сервер уходят только `lookup_id` и public key; phrase, entropy и private key
не входят в payload, logs или browser storage.

Login/recovery выполняется через `/api/v1/auth/challenges` и точную бинарную
подпись transcript `LUMI-AUTH-V1`. Challenge живёт не более пяти минут,
ограничен числом попыток и атомарно потребляется после успешной проверки.

Mutating account-owned routes требуют:

- действующую `HttpOnly` session cookie;
- exact `Origin` или `Referer` из `LUMI_WEB_ORIGIN`;
- session-bound `X-Lumi-CSRF`, доступный browser из `lumi_csrf` cookie.

## Migrations и deploy

Production не запускает migrations из каждого server instance. Сначала
отдельным deploy step выполняется:

```sh
DATABASE_URL=postgres://... make db-migrate
```

и только после успешного завершения переключается traffic на новый binary.
Migration files после попадания в shared branch не изменяются. Rollback —
предыдущий совместимый binary или новая forward repair migration.

## Проверка

Основной gate:

```sh
make c
make web-e2e
```

API tests проверяют replay challenge, session revocation, CSRF, duplicate
idempotency key, stale revision и маскировку чужих объектов как `404`.
Ручная restart-проверка: зарегистрировать аккаунт, перезапустить
`make server-r` без удаления PostgreSQL и открыть `/api/v1/account/me` с прежней
cookie. Ответ должен сохранить тот же `user_id` и профиль.

Остановить локальную базу:

```sh
make db-down
```
