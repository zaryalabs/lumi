# S1 Web EPUB Reader: план до работоспособной версии

Status: `active`

## Scope

Этот временный план описывает переход от текущего S0/S1 scaffold и статического
UI/UX-прототипа к работоспособной первой web-версии Lumi, визуально и
поведенчески близкой к прототипу.

Целевой продуктовый срез — `S1 Web EPUB Reader` из
[`../early-slices.md`](../early-slices.md). План реализует принятые решения из:

- [`../vision.md`](../vision.md);
- [`../systems/feature-registry.md`](../systems/feature-registry.md);
- [`../systems/backend-api.md`](../systems/backend-api.md);
- [`../systems/web-account.md`](../systems/web-account.md);
- [`../systems/normalized-content.md`](../systems/normalized-content.md);
- [`../systems/reader-architecture.md`](../systems/reader-architecture.md);
- [`../systems/reading-screen.md`](../systems/reading-screen.md);
- [`../systems/formats/epub.md`](../systems/formats/epub.md);
- [`../systems/security-privacy.md`](../systems/security-privacy.md);
- [`../systems/quality.md`](../systems/quality.md);
- [`../visuals/reader-first-direction.md`](../visuals/reader-first-direction.md);
- [`../visuals/prototype/`](../visuals/prototype/).

Этот документ не меняет canonical product или architecture decisions. Новые
долговечные решения по auth, schema, anchors, sync, blob protocol и pagination
должны быть оформлены в соответствующих документах и ADR.

## Целевой пользовательский путь

Работоспособный S1 должен позволять пользователю:

1. Создать web-аккаунт и позднее войти в него.
2. Загрузить настоящий DRM-free reflowable EPUB.
3. Увидеть прогресс импорта или понятную диагностируемую ошибку.
4. Найти материал в библиотеке, открыть, архивировать, восстановить, удалить и
   скачать исходник.
5. Читать материал постранично, использовать TOC, внутренние ссылки и сноски.
6. Изменять тему, размер текста, ширину строки и другие базовые настройки.
7. Выделить текст, создать highlight или note, отредактировать и удалить их.
8. Закрыть браузер, вернуться и получить прежние материалы, позицию чтения,
   highlights и notes.
9. Экспортировать annotations с цитатой, metadata источника и полным anchor.
10. Использовать основные сценарии на desktop и mobile viewport.

## Не входит в S1

- PDF, FB2, web capture, Telegram, X, Markdown и `lum`.
- AI, agents, MCP, карточки и generated artifacts.
- Learning, challenges, FSRS и explain-back.
- Global search, RAG и personal knowledge base.
- Social и shared reading.
- Obsidian integration и plugin runtime.
- Native full-copy sync UX, offline web и native clients.
- Fixed-layout EPUB и DRM.

Прототипные действия «Объяснить» и «Карточка» до реализации соответствующих
подсистем должны быть скрыты через capability flag или отсутствовать в
пользовательском UI. Неработающие controls не должны выглядеть доступными.

## Текущее состояние и основной разрыв

В репозитории уже есть:

- shared domain contracts для material, revision, normalized package, reader,
  anchors, annotations, progress, blobs и jobs;
- fixture-backed EPUB import path;
- Axum `/api/v1` routes для предварительных account, material, reader,
  annotation, progress, export и job boundaries;
- Dioxus Web shell;
- статический reader-first UI/UX-прототип и Playwright-сценарии для него;
- unit и API tests для fixture-backed S0/S1 contracts.

До настоящего продукта не хватает следующих сквозных частей:

- Dioxus Web строит данные прямо из fixtures и не является API client;
- серверное состояние хранится в памяти и теряется после рестарта;
- importer принимает fixture model, а не реальный EPUB upload;
- нет production auth/session flow и account isolation middleware;
- нет SQLx migrations, PostgreSQL repositories и durable change records;
- нет реального blob storage и durable background jobs;
- нет browser selection adapter, pagination/page map и annotation overlays;
- рабочий web UI пока не перенёс UX и визуальный язык прототипа;
- browser tests рабочего приложения не покрывают полный пользовательский путь.

## Рабочие потоки

### Продукт и UX

- Зафиксировать onboarding, регистрацию, login, recovery и потерю сессии.
- Описать empty, loading, importing, ready, failed, archived, deleted, saving,
  saved и save-failed states.
- Уточнить archive, delete, restore и повторный импорт того же EPUB.
- Определить scope reader settings: account-wide или device-specific.
- Уточнить значение progress, текущей главы и приблизительного времени чтения.
- Не показывать «Всё сохранено», пока UI не получает реальный sync/save state.
- Дополнить прототип отсутствующими error, retry, session-expired и permission
  states до их переноса в Dioxus.

### Frontend

- Перенести visual tokens и компоненты прототипа в Dioxus без копирования его
  fixture state model.
- Добавить маршруты библиотеки и reader с прямыми URL материала.
- Реализовать versioned API client для `/api/v1`.
- Добавить async loading, error boundaries, retry и session-expired handling.
- Реализовать настоящий file picker/upload и отображение import job states.
- Сделать библиотеку полностью server-backed.
- Реализовать reader render adapter над `ReadingDocument`.
- Добавить TOC, links, footnotes, page navigation и reader settings.
- Реализовать browser `Selection`/`Range` adapter и mapping DOM ranges в
  source-backed anchors.
- Реализовать highlight overlays, note editor и annotations panel.
- Добавить optimistic commands с durable server acknowledgement и обработкой
  revision conflicts.
- Сохранить семантические landmarks, roles, labels, keyboard navigation, focus
  states и mobile notes sheet из прототипа.

### Backend и application layer

- Ввести application services и repository boundaries вместо прямого доступа
  handlers к in-memory maps.
- Добавить SQLx migrations и PostgreSQL persistence.
- Реализовать production auth boundary, sessions, revocation и account scoping.
- Добавить idempotency keys для upload и mutation commands.
- Реализовать multipart upload и durable import job lifecycle.
- Хранить source EPUB, normalized package, source map и diagnostics.
- Реализовать local/dev blob backend и S3-compatible production backend за
  одним contract.
- Сохранять materials, immutable revisions, annotations, progress и change
  records в общей sync-ready domain model.
- Добавить optimistic concurrency для редактируемых entities.
- Нормализовать API errors и не включать пользовательский контент в logs.
- Добавить readiness, structured tracing и job observability.

### EPUB import и reader core

- Реализовать безопасное чтение ZIP/EPUB container и package metadata.
- Разобрать OPF, manifest, spine, nav/NCX, metadata и resources.
- Санитизировать XHTML/SVG-like content и переписать resource URLs.
- Нормализовать headings, paragraphs, lists, blockquotes, figures, links,
  tables, code и footnotes в поддерживаемые `ReadingNode`.
- Создавать стабильные node/block ids, fingerprints и source map.
- Сохранять structured diagnostics для частично поддержанных и повреждённых
  файлов.
- Разделить import, persisted normalized package, `ReadingDocument`, render
  plan и platform adapter.
- Реализовать pagination через browser layout measurement и `PageMap`, не
  создавая собственный text layout engine.
- Виртуализировать длинные документы и не держать всю книгу в активном DOM.
- Реализовать anchor creation, resolution и базовый recovery path.

### Security, platform и эксплуатация

- Ограничить upload size, expanded ZIP size, compression ratio и число файлов.
- Защитить import от path traversal, ZIP bombs, scripts, external resources и
  HTML/SVG injection.
- Добавить secure session cookies, CSRF protection и rate limits.
- Проверить tenant isolation для каждого account-owned route.
- Настроить development, test, staging и production configuration.
- Добавить database migrations при deploy, backups и проверяемое восстановление.
- Добавить health/readiness checks, structured logs и минимальные metrics.
- Зафиксировать privacy UX для cloud-backed личного контента.

### Quality

- Собрать golden EPUB corpus: простой текст, TOC, images, footnotes, tables,
  CSS edge cases, malformed и malicious cases.
- Добавить domain tests для anchors, progress и annotation conflicts.
- Добавить import snapshot/compatibility tests для normalized packages.
- Добавить repository и migration integration tests с PostgreSQL.
- Добавить API tests для auth, idempotency, account isolation и persistence.
- Добавить Playwright journey: register/login → upload → import → read → note →
  reload → export.
- Проверить mobile viewport, keyboard navigation, roles, labels, focus и
  contrast.
- Проверить performance budgets из `systems/quality.md` на normalized open,
  import responsiveness и reader navigation.

## Порядок реализации

### Этап 0. Закрыть блокирующие решения — выполнен

- Провести auth spike и принять ADR, заменяющий или уточняющий временный
  seed-auth boundary.
- Зафиксировать PostgreSQL schema, migration policy и минимальную sync-ready
  change model.
- Провести pagination spike на длинной главе, изображениях, таблице и сносках.
- Выбрать EPUB parser, sanitizer и ZIP limits.
- Превратить этапы этого плана в issues с registry IDs.

Результаты:

- auth boundary: [`../adr/0003-seed-derived-challenge-auth.md`](../adr/0003-seed-derived-challenge-auth.md),
  исполняемый spike `spikes/stage0/src/auth.rs`;
- PostgreSQL и change model:
  [`../adr/0004-postgresql-sync-ready-schema.md`](../adr/0004-postgresql-sync-ready-schema.md);
- EPUB stack/limits:
  [`../adr/0005-epub-import-stack-and-limits.md`](../adr/0005-epub-import-stack-and-limits.md),
  исполняемый spike `spikes/stage0/src/epub.rs`;
- pagination:
  [`../adr/0006-browser-measured-pagination.md`](../adr/0006-browser-measured-pagination.md),
  browser-spike `../visuals/pagination-spike/`;
- issue backlog этапов 1–7: [`s1-issues.md`](s1-issues.md).

Критерий завершения: приняты необходимые ADR, а auth, EPUB и pagination риски
проверены исполняемыми spikes или fixtures.

### Этап 1. Persistent account slice — выполнен

- Добавить SQLx migrations и repositories.
- Реализовать account, auth verifier, session и device records.
- Добавить registration, login, logout, recovery и session revocation.
- Защитить account-owned API routes auth middleware и owner scoping.
- Подключить минимальный account UI.

Критерий завершения: аккаунт и сессия переживают рестарт, а пользователь не
может прочитать или изменить объекты другого аккаунта.

Результат:

- добавлена PostgreSQL-схема аккаунтов, auth, sessions, devices, sync и
  account-owned данных с отдельной deploy-time утилитой миграций;
- регистрация, вход, recovery, logout и отзыв сессий используют локально
  производимый Ed25519 proof без передачи recovery-фразы серверу;
- account-owned API закрыт session middleware, CSRF-защитой и owner scoping;
- минимальный Dioxus account UI проходит browser flow регистрации, выхода и
  повторного входа;
- restart persistence, challenge replay, session revocation, idempotency и
  межаккаунтная изоляция покрыты тестами;
- локальный запуск и модель безопасности описаны в
  [`docs/runbooks/persistent-account.md`](../runbooks/persistent-account.md).

### Этап 2. Real EPUB import slice — выполнен

Реализовать сквозной путь:

```text
Upload -> source blob -> durable Job -> EPUB importer
       -> DocumentRevision -> Normalized Content Package
       -> ReadingDocument -> library entry
```

- Добавить upload API и job progress/error endpoints.
- Реализовать безопасный EPUB import и normalized package persistence.
- Сохранять source EPUB, resources, source map и diagnostics.
- Добавить cancellation/retry и восстановление незавершённых jobs.
- Отобразить failed import как диагностируемое состояние библиотеки.

Критерий завершения: поддерживаемые реальные EPUB импортируются после restart,
а повреждённый или неподдержанный EPUB выдаёт понятный failed state.

Результат:

- добавлены multipart upload, content-addressed local blob backend и scoped
  source download через общий `BlobStore` contract;
- durable PostgreSQL worker сохраняет queued/running/succeeded/failed/cancelled,
  attempt, cancellation, retry и startup recovery;
- real EPUB importer разбирает OCF, OPF, spine, EPUB 3 nav/EPUB 2 NCX, metadata
  и resources, применяет лимиты ADR 0005 и строит typed `ReadingNode`;
- immutable revision, normalized package, source map, blob manifest и structured
  diagnostics публикуются атомарно и доступны после restart;
- минимальный Dioxus upload/status UI показывает supported и failed imports,
  diagnostic codes, cancel/retry и сохраняет состояние после browser reload;
- решение по worker/blob lifecycle закреплено в
  [`../adr/0007-durable-import-jobs-and-blob-store.md`](../adr/0007-durable-import-jobs-and-blob-store.md),
  local workflow — в
  [`../runbooks/real-epub-import.md`](../runbooks/real-epub-import.md).

### Этап 3. API-backed библиотека

- Перенести library UI прототипа в Dioxus.
- Добавить empty state, upload dialog и material cards.
- Показывать queued, importing, ready и failed materials.
- Реализовать material details, archive, restore, delete и source download.
- Удалить fixture construction из production web path.

Критерий завершения: библиотека полностью строится из `/api/v1` и сохраняет
состояние после перезагрузки browser и server.

### Этап 4. Рабочий reader

- Добавить material reader route и загрузку `ReadingDocument`.
- Реализовать render plan и Dioxus/DOM platform adapter.
- Добавить TOC, internal links, footnotes и navigation history.
- Реализовать page-like navigation и browser-measured `PageMap`.
- Добавить lazy rendering и кеширование page map.
- Реализовать reader settings и восстановление последней позиции.
- Обеспечить desktop и mobile reader layouts.

Критерий завершения: реальный EPUB читается от начала до конца через shared
reader model без рендера исходного EPUB XHTML/CSS как product model.

### Этап 5. Annotations и progress

- Реализовать selection-to-anchor flow.
- Добавить создание, редактирование и удаление highlight и note.
- Добавить overlay rendering и переход из notes panel к anchor.
- Добавить optimistic UI, server acknowledgement и conflict handling.
- Сохранять reading progress и восстанавливать последнюю позицию.
- Реализовать portable annotation export.

Критерий завершения: position, highlights и notes переживают закрытие браузера
и рестарт сервера, а изменение reader settings не ломает привязки.

### Этап 6. Сведение с UI/UX-прототипом

- Перенести visual tokens, typography, spacing, cards, dialogs и reader chrome.
- Подключить реальные save/sync indicators.
- Добавить mobile panels и bottom sheet behavior.
- Завершить empty, loading, error, retry и expired-session states.
- Провести визуальное и accessibility сравнение с прототипом.
- Удалить или скрыть controls отложенных подсистем.

Критерий завершения: основные desktop и mobile user journeys визуально и
поведенчески соответствуют reader-first прототипу.

### Этап 7. Hardening и закрытая beta

- Закрыть golden fixture, security и compatibility suites.
- Добавить полный browser E2E пользовательский путь.
- Проверить account isolation, session handling и malicious import cases.
- Проверить performance budgets на больших EPUB и библиотеках.
- Настроить staging, migrations, monitoring, backups и restore drill.
- Проверить export/download и privacy UX.

Критерий завершения: выполнены общие completion criteria ниже, а `make c` и
обязательные web E2E проходят в поддерживаемом окружении.

## Критический путь

```text
Auth и persistence
  -> real EPUB import
  -> API-backed library
  -> paginated reader
  -> selection и annotations
  -> UI/UX convergence
  -> beta hardening
```

После стабилизации domain и API contracts параллельно могут развиваться:

1. auth, SQLx, repositories, jobs и blobs;
2. EPUB importer, normalized package, reader core и anchors;
3. Dioxus library, reader adapter и перенос prototype UI;
4. fixtures, Playwright, security, CI и deployment.

## Completion criteria

План считается выполненным, когда:

- новый пользователь может зарегистрироваться или войти и получить своё
  изолированное cloud-backed пространство;
- настоящий поддерживаемый EPUB загружается и импортируется через durable job;
- библиотека показывает persisted materials и честные import/save states;
- плохой EPUB возвращает структурированную и понятную диагностику;
- reader использует `ReadingDocument`, render plan и platform adapter;
- TOC, page navigation, links, footnotes и settings работают;
- highlights, notes и progress сохраняются после browser/server restart;
- annotations используют полный source-backed anchor, а не DOM path;
- source EPUB и portable annotations можно скачать;
- desktop и mobile flows соответствуют прототипу и доступны с клавиатуры;
- account isolation, import security и persistence покрыты tests;
- golden EPUB compatibility tests и основной Playwright journey проходят;
- staging deployment имеет migrations, readiness, logs, backups и проверяемое
  восстановление;
- выполнены `make c` и `make web-e2e` в локально поддерживаемом окружении.

После выполнения S1 дальнейшая функциональность выбирается из
[`../systems/feature-registry.md`](../systems/feature-registry.md) и порядка,
зафиксированного в [`../early-slices.md`](../early-slices.md), без расширения
этого временного плана до общего roadmap Lumi.
