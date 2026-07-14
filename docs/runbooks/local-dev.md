# Локальная разработка

Статус: исполняемый

Этот runbook описывает основной Docker-first запуск полного локального стека и
расширенный host-native workflow для разработки Rust, Dioxus и Playwright.

## Основной Docker-first запуск

Для обычного запуска нужен только Docker с Compose:

```sh
make up
```

Команда собирает образы и ждёт readiness полного стека: PostgreSQL, одноразовой
migration, blob volume, Axum server и nginx с Dioxus Web. Эквивалентная команда
без Make:

```sh
docker compose up -d --build --wait
```

Локальные адреса публикуются только на loopback:

- Web: `http://127.0.0.1:5173`
- API health: `http://127.0.0.1:8080/api/v1/health`
- PostgreSQL: `127.0.0.1:5432`

Браузер использует relative `/api/v1` через nginx, поэтому web и API работают
в одном origin. Порты можно переопределить переменными `LUMI_WEB_PORT`,
`LUMI_SERVER_PORT` и `LUMI_POSTGRES_PORT`.

Логи, остановка и очистка:

```sh
make logs
make down
make reset
```

`make down` сохраняет named volumes. `make reset` намеренно удаляет PostgreSQL и
blob-данные; используйте его только когда нужен чистый локальный старт.

## Расширенный host-native workflow

Host-native запуск удобен при активной разработке. Для него нужны:

- Rust 1.88+ с `cargo`, `rustfmt` и `clippy`;
- target `wasm32-unknown-unknown`;
- Dioxus CLI `dx` версии, совместимой с Dioxus 0.7;
- Node.js и npm для Playwright;
- Docker с Compose для PostgreSQL;
- `pre-commit` для Git hooks.

Полезные команды установки:

```sh
rustup target add wasm32-unknown-unknown
cargo install dioxus-cli --version 0.7.9 --locked
python -m pip install pre-commit
```

Dioxus также можно установить официальным prebuilt installer или через
`cargo-binstall`.

### Bootstrap

```sh
make init
```

`make init` устанавливает pre-commit hooks, загружает Cargo dependencies,
добавляет wasm target через `rustup`, устанавливает Playwright dependencies и
запускает `dx doctor`, когда соответствующие инструменты доступны.

### Локальные процессы

Для host-native процессов поднимите только PostgreSQL и примените migrations:

```sh
make db-up
make db-migrate
```

Подробности — в [persistent-account.md](persistent-account.md).

Настройка real EPUB import, blob root и restart recovery описана в
[real-epub-import.md](real-epub-import.md).

Проверка API-backed библиотеки, lifecycle-команд и source download описана в
[api-backed-library.md](api-backed-library.md).

Запустите API:

```sh
make server-r
```

Перед компиляцией `server-r` проверяет только доступность TCP-порта PostgreSQL.
Схему по-прежнему готовит отдельная команда `make db-migrate`.

В другом терминале запустите web shell:

```sh
make web-r
```

Значения по умолчанию:

- API: `http://127.0.0.1:8080/api/v1`
- Web: `http://127.0.0.1:5173`
- bind API: `LUMI_SERVER_BIND`
- endpoint API при build/serve web: `LUMI_API_BASE`
- host web: `LUMI_WEB_HOST`
- port web: `LUMI_WEB_PORT`

## Проверки качества

Быстрая проверка:

```sh
make l
```

Rust-тесты:

```sh
make t
```

Полная проверка перед handoff:

```sh
make c
```

Browser-тест для web-изменений:

```sh
make web-e2e
```

`make c` по умолчанию не запускает Playwright. Выполняйте `make web-e2e`, если
изменение затрагивает browser behavior, accessibility, routing или reader.

Рабочий reader, его API и ручная проверка описаны в
[`working-reader.md`](working-reader.md).

## Режимы browser-проверки

Автоматический Playwright:

```sh
make web-e2e
```

Реальный локальный профиль для host-native процессов:

```sh
make server-r
make web-r
LUMI_E2E_REAL_PROFILE=1 PLAYWRIGHT_BASE_URL=http://127.0.0.1:5173 npm --prefix tests/e2e test
```

Agent/operator inspection:

```sh
make agent-inspect
```

Сохраняйте важные наблюдения в
[docs/tmp-plans/playwright-agent-inspection.md](../tmp-plans/playwright-agent-inspection.md).

## Диагностика

- Если Dioxus web lint пропущен, установите `wasm32-unknown-unknown`.
- Если `make web-r` не находит `dx`, установите Dioxus CLI.
- Если Playwright не находит браузеры, выполните
  `npm --prefix tests/e2e run install-browsers`.
- Если `make init` не может установить инструменты внутри restricted sandbox,
  выполните напечатанные команды в обычном developer shell.
- Состояние контейнеров видно через `docker compose ps`; подробности запуска —
  через `make logs`.
