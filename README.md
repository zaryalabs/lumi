# Lumi

Lumi — open-source приложение для вдумчивого чтения и обучения на материалах,
которые пользователь уже выбрал: книгах, статьях, тредах, сообщениях и заметках.

Каноническое направление продукта для разработки описано в
[docs/vision.md](docs/vision.md), а принятый технический дизайн `v01` — в
[docs/systems](docs/systems).

## Текущее состояние

В репозитории реализован baseline S1 Web Reader:

- Rust workspace;
- общие доменные контракты в `crates/lumi-core`;
- Axum API в `crates/lumi-server`;
- Dioxus web-приложение в `apps/web`;
- Playwright E2E coverage в `tests/e2e`;
- цели `make` и pre-commit hooks для локальных quality gates.

Текущая цель реализации — срез S1 Web Reader из
[docs/early-slices.md](docs/early-slices.md). EPUB остаётся полным эталонным
импортёром, а публичные web URL и приём текста/ссылок через Telegram-бота входят
как намеренно узкие baseline-источники. Реализованы постоянные аккаунты,
durable-импорт реальных EPUB, полностью API-backed библиотека, рабочий
browser-measured пагинированный reader, durable-аннотации, progress UX и общий
baseline приёма источников из Web/Telegram. Web-библиотека и reader используют
reader-first визуальную систему paper/sage на desktop и touch layouts, включая
реальные save states, keyboard flows модальных окон и панелей, capability-aware
source UI и восстановление истёкшей сессии. Реализован repository-side baseline
beta hardening: server-side continuation projection, граница Telegram webhook,
корпуса security и compatibility, performance budgets, воспроизводимый staging
image, readiness/alerts и проверенные инструменты backup/restore для PostgreSQL
и blob-данных. Внешний staging deployment, TLS/DNS, регистрация provider и
operator acceptance зависят от окружения; см.
[docs/runbooks/beta-staging.md](docs/runbooks/beta-staging.md).

## Локальный запуск

Для основного пути нужны Docker с Compose и `make`. Он собирает и запускает
PostgreSQL, migrations, API и web-приложение:

```sh
make up
```

После readiness откройте <http://127.0.0.1:5173>. Web обращается к API через
same-origin путь `/api/v1`; прямой health API доступен только с локальной машины
по адресу <http://127.0.0.1:8080/api/v1/health>.

Управление стеком:

```sh
make logs  # поток логов
make down  # остановка с сохранением PostgreSQL и blob-данных
make reset # остановка и явное удаление локальных данных
```

Без `make` используйте `docker compose up -d --build --wait`.
Host-native workflow для разработки Rust/Dioxus и установка `cargo`, `dx`,
Node.js и `pre-commit` описаны в
[docs/runbooks/local-dev.md](docs/runbooks/local-dev.md).

Основные проверки:

```sh
make l
make t
make c
```

Browser E2E после установки локальных Playwright-зависимостей:

```sh
make web-e2e
```

Подробности — в [docs/runbooks/local-dev.md](docs/runbooks/local-dev.md).

## Работа с документацией

В репозитории временно используется единое русскоязычное дерево документации:

- [`docs`](docs) — канонические документы продукта, архитектуры, ADR и runbooks.
- [`docs/tmp-plans`](docs/tmp-plans) — временные планы реализации активных
  промежуточных срезов.

Порядок работы:

1. Обсуждать, оформлять и стабилизировать продуктовые и архитектурные решения на
   русском языке в `docs/`.
2. Хранить долгоживущие решения в канонических разделах, прежде всего в
   `docs/systems/`, `docs/adr/` и `docs/runbooks/`.
3. Не считать временные планы каноническими. Долгоживущее решение из временного
   плана переносить в соответствующий канонический документ.

## Структура репозитория

```text
apps/web/             Dioxus web shell и слой platform adapter
crates/lumi-core/     общие доменные контракты
crates/lumi-server/   граница Axum API и точка входа server
docs/                 канонические документы продукта, систем, ADR и runbooks
docs/visuals/         статический UI/UX prototype без зависимостей и заметки
docs/tmp-plans/       временные планы реализации
tests/e2e/            browser-тесты Playwright и agent inspection harness
```

Список поддерживаемых локальных команд выводит `make help`.

Для быстрой итерации UI/UX без Rust web stack и backend выполните
`make prototype-r` и откройте <http://127.0.0.1:4173>. Workflow прототипа описан
в [`docs/visuals`](docs/visuals).
