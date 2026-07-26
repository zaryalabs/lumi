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

Текущая реализованная основа — S1 Web Reader. EPUB остаётся полным эталонным
импортёром, а публичные web URL и приём текста/ссылок через Telegram-бота входят
как намеренно узкие baseline-источники. Реализованы постоянные аккаунты,
durable-импорт реальных EPUB, Markdown и portable `.lum` packages, полностью
API-backed библиотека, рабочий
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
Repository-side production contract и main-only self-hosted CI/CD описаны в
[docs/runbooks/production-deploy.md](docs/runbooks/production-deploy.md). Его
наличие не означает, что server bootstrap, DNS или первый production deploy уже
выполнены.

Для `0.2.0` реализованы foundation A1 и все продуктовые эпики E1–E4:
owner-scoped explicit source context, зашифрованный OpenRouter BYOK,
глобальный durable streaming AI-чат, Reader selection handoff и рабочие
source citations, а также durable AI queue, внутренний worker, сохранённые
chapter/material summaries и manual-edit candidate policy. Revocable
account-scoped MCP Streamable HTTP предоставляет product tools и fenced
external AI worker поверх тех же application services. Сокращение публикуется
как отдельный portable `.lum`: сервер собирает и повторно импортирует package,
атомарно добавляет готовый производный материал в библиотеку и сохраняет точные
ссылки на immutable revision оригинала. Операционные
контракты описаны в
[docs/runbooks/ai-persistence.md](docs/runbooks/ai-persistence.md) и
[docs/runbooks/personal-ai-assistant.md](docs/runbooks/personal-ai-assistant.md),
[docs/runbooks/ai-task-queue.md](docs/runbooks/ai-task-queue.md) и
[docs/runbooks/derived-materials.md](docs/runbooks/derived-materials.md).
Настройка внешнего агента описана в
[`docs/runbooks/mcp-external-agents.md`](docs/runbooks/mcp-external-agents.md).

Для `0.3.0/E1` реализован первый deterministic learning vertical без
обязательного AI provider: durable completion после сохранения reading progress,
одно необязательное предложение в Reader, ручные versioned questions,
детерминированная проверка закрытых ответов и explicit self-check открытых,
immutable session snapshots, durable attempts, source jump и продолжение после
reload. В карточке материала доступен ручной вход в learning, а сервер публикует
capability `learning-core`. Контракт описан в
[`docs/adr/0026-learning-completion-items-sessions.md`](docs/adr/0026-learning-completion-items-sessions.md).

Для `0.3.0/E2` реализованы ordered hints/source assistance evidence,
versioned FSRS scheduling, атомарное обновление attempt + schedule и bounded
экран `Челленджи` с `Сегодня`/`Закрепить сейчас`. Global scheduling,
manual-only, material pause/resume и session snooze сохраняют историю и не
создают штрафной backlog. Persistent server публикует capability
`learning-scheduling`; контракт и проверка описаны в
[`ADR 0027`](docs/adr/0027-fsrs-scheduling-challenges.md) и
[`learning runbook`](docs/runbooks/learning.md).

Для `0.3.0/E3` реализованы source-backed AI generation и open-answer
evaluation поверх общей очереди/provider/context contracts `0.2.0`. Generated
items проходят строгую schema/citation validation и появляются только как
редактируемые черновики. Text explain-back использует durable learning session
и последовательные immutable evaluation artifacts с
`understood`/`partial`/`needs_review`/`not_evaluated`; при отсутствии provider
остаётся честная self-check ветка. Решение описано в
[`ADR 0028`](docs/adr/0028-learning-ai-evaluation-explain-back.md).

Для `0.3.0/E4` реализован сквозной голосовой ответ: Web запрашивает микрофон
только по действию пользователя, даёт прослушать и удалить локальную запись,
загружает её как owner-scoped `AudioAttachment` и отправляет server-side в
OpenAI Audio Transcriptions API (`whisper-1`) с отдельным зашифрованным BYOK.
Транскрипт можно исправить и нужно явно подтвердить до оценки; повтор
транскрибации использует тот же attachment, а retention удаляет исходное аудио
после acceptance. Обычный текстовый ответ всегда доступен.

Для `0.3.0/E5` реализован platform/release slice: account-scoped MCP tools
`list_learning_items`, `create_flashcard_task` и `submit_learning_answer`
используют те же learning application services, AI queue, authorization и
idempotency, что HTTP/Web. Добавлены capability filtering, frozen MCP schema
`mcp-tools.v2`, typed limits/errors, owner/parity fixtures, payload-free
операционные traces и process/account limits для AI/transcription. Изолированный
release gate подтверждает fresh migrations, PostgreSQL/compatibility/security/
performance suites, `make c`, Web E2E и staging image smoke. Workspace и Web
package имеют версию `0.3.0`; внешний staging operator acceptance остаётся
отдельным gate окружения.

Для `0.4.0/E1` реализованы Records v2 и Rich Reader: прежние annotations
совместимо декодируются и мигрируются в единый контракт с explicit targets,
title/tags/status и relation seam. Reflowable и PDF Reader поддерживают
yellow/bold highlights, изменение стиля, selection notes и keyboard-доступные
записи на полях, durable CRUD/navigation и portable
`lumi.annotations.v2` export. Решение и ручная проверка описаны в
[`ADR 0030`](docs/adr/0030-annotation-v2-rich-reader.md) и
[`durable annotations runbook`](docs/runbooks/durable-annotations.md).

Для `0.4.0/E2` реализованы Voice Notes и внутренние связи: Reader переиспользует
общий browser recorder и owner-scoped audio attachment lifecycle, поддерживает
preview/file fallback, durable playback, safe delete/refcount и manifest-only
export. Markdown-заметки получают stable `LinkTarget`, wikilink suggestions,
unresolved/ambiguous resolution и backlinks без зависимости от Obsidian.
Решения описаны в [`ADR 0031`](docs/adr/0031-voice-note-audio-lifecycle.md),
[`ADR 0032`](docs/adr/0032-stable-record-links-backlinks.md) и
[`runbook`](docs/runbooks/voice-notes-and-links.md).

Для `0.4.0/E3` реализовано поисковое ядро: versioned source-aware chunks для
normalized materials/PDF, Annotation v2, accepted AI artifacts и active
learning items; Tantivy BM25 с проверяемым fastText rerank; транзакционные
`search_index` jobs, incremental replace/delete, restart recovery и full
rebuild. Owner-filtered `/search`, `/search/retrieve`, `/search/status` и
`/search/rebuild` возвращают plain snippets, exact open targets и compatible
citations. Model/checksum failure явно отключает query capabilities без
BM25-only деградации. Решение и эксплуатация описаны в
[`ADR 0033`](docs/adr/0033-search-chunks-tantivy-fasttext.md) и
[`search runbook`](docs/runbooks/search-index.md).

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

Единый Web roadmap теперь сгруппирован в 18 крупных продуктовых эпиков:
четыре для `0.2.0`, по пять для `0.3.0` и `0.4.0`, четыре для `0.5.0`.
Изолированный последовательный запуск эпиков через Codex CLI описан в
[docs/runbooks/codex-plan-runner.md](docs/runbooks/codex-plan-runner.md).

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
