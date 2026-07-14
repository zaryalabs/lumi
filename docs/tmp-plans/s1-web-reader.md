# S1 Web Reader: EPUB, Web и Telegram baseline

Status: `active`

## Scope

Этот временный план описывает переход от текущего S0/S1 scaffold и статического
UI/UX-прототипа к работоспособной первой web-версии Lumi. EPUB остается
полным reference importer, а публичные web URL и текстовые Telegram-сообщения
входят в тот же срез как узкие, но реальные source paths.

Целевой продуктовый срез — `S1 Web Reader` из
[`../early-slices.md`](../early-slices.md). План реализует принятые решения из:

- [`../vision.md`](../vision.md);
- [`../systems/feature-registry.md`](../systems/feature-registry.md);
- [`../systems/backend-api.md`](../systems/backend-api.md);
- [`../systems/web-account.md`](../systems/web-account.md);
- [`../systems/normalized-content.md`](../systems/normalized-content.md);
- [`../systems/reader-architecture.md`](../systems/reader-architecture.md);
- [`../systems/reading-screen.md`](../systems/reading-screen.md);
- [`../systems/formats/epub.md`](../systems/formats/epub.md);
- [`../systems/formats/web-reader.md`](../systems/formats/web-reader.md);
- [`../systems/formats/telegram.md`](../systems/formats/telegram.md);
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
11. Вставить публичный HTTP/HTTPS URL, дождаться text-first импорта
    статьи и открыть ее в общем reader.
12. Привязать Telegram-бота, отправить или переслать текст и увидеть
    один durable material в своей web-библиотеке; обычную web-ссылку
    из бота обработать тем же web import path.

## Не входит в S1

- PDF, FB2, X, Markdown и `lum`.
- Cloud browser, browser extension, authenticated/JS-only pages, site-specific
  web adapters, recapture/diff и полная resource fidelity.
- Telegram media, captions, files, batches, public `t.me` hydration, automatic
  grouping, delivery state по устройствам и Mini App.
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
- domain enums, source refs, job stages и worker dispatch пока прошиты под
  EPUB и не могут принять web/Telegram source без обобщения;
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
- Добавить в общий dialog добавления материала URL input и понятные
  ограничения baseline web capture.
- Описать connect, connected, expired-token, unlink и unsupported-message
  states для Telegram-бота.
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
- Добавить URL import в общий add-material flow и Telegram pairing
  section в аккаунте.
- Показывать source kind, canonical URL/Telegram attribution и общие
  queued/importing/ready/failed states без отдельных библиотек.
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
- Обобщить `ImportSourceKind`, typed source refs, worker dispatch и общую
  публикацию reflowable import result без сложной plugin-системы.
- Использовать existing durable `import_jobs` как baseline-проекцию
  account `ImportInbox` для всех трех source paths.
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

### Web и Telegram source baseline

- Добавить `web_page` и `telegram` в `MaterialKind`, `SourceFormat`, source
  identity, source map и typed source locator contracts.
- Зафиксировать compatibility impact расширения normalized package/source
  locator в ADR до миграции persisted schema.
- Принимать публичный HTTP/HTTPS URL, выполнять bounded raw fetch и
  оборачивать результат в immutable baseline `RenderedPageSnapshot`.
- Извлекать title, canonical URL, author и semantic `article`/`main`; мапить
  headings, paragraphs, lists, blockquotes, code и links в `ReadingNode`.
- Сохранять source snapshot и metadata как blobs/provenance; unsupported
  resources пропускать или заменять placeholder с diagnostic.
- Добавить short-lived one-time Telegram pairing token, связь Telegram
  identity с account, unlink и idempotency log по `update_id`.
- Вынести Telegram update handling в transport-neutral service; для локальной
  пробы дать long-polling runner, а перед публичной beta подключить тот же
  handler к webhook с secret token.
- Принимать direct text и forwarded text как один material на message;
  если message состоит из одного поддержанного web URL, передавать
  его в тот же web import path.
- Отвечать в боте на `/start`, `/help`, `/unlink`, successful acceptance и
  unsupported input; не обещать поддержку файлов/media/batches.

### Security, platform и эксплуатация

- Ограничить upload size, expanded ZIP size, compression ratio и число файлов.
- Защитить import от path traversal, ZIP bombs, scripts, external resources и
  HTML/SVG injection.
- Защитить URL import от SSRF: запретить private/link-local/loopback/cloud
  metadata addresses, повторять проверку после DNS resolution и redirects,
  ограничить response size, redirects и timeout.
- Хранить Telegram bot token как runtime secret, pairing token — только как
  hash с expiry; не писать message body и raw update в logs.
- Добавить secure session cookies, CSRF protection и rate limits.
- Проверить tenant isolation для каждого account-owned route.
- Настроить development, test, staging и production configuration.
- Добавить database migrations при deploy, backups и проверяемое восстановление.
- Добавить health/readiness checks, structured logs и минимальные metrics.
- Зафиксировать privacy UX для cloud-backed личного контента.

### Quality

- Собрать golden EPUB corpus: простой текст, TOC, images, footnotes, tables,
  CSS edge cases, malformed и malicious cases.
- Добавить committed HTML snapshots для article/main, metadata, code/lists и bad
  extraction; tests не должны зависеть от live sites.
- Добавить Telegram update fixtures для pairing, direct/forwarded text, web link,
  duplicate `update_id`, unpaired sender и unsupported input.
- Добавить domain tests для anchors, progress и annotation conflicts.
- Добавить import snapshot/compatibility tests для normalized packages.
- Добавить repository и migration integration tests с PostgreSQL.
- Добавить API tests для auth, idempotency, account isolation и persistence.
- Добавить Playwright journeys: register/login → upload EPUB → import → read →
  note → reload → export и save fixture URL → import → read.
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
- issue backlog этапов 1–8: [`s1-issues.md`](s1-issues.md).

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

### Этап 3. API-backed библиотека — выполнен

- Перенести library UI прототипа в Dioxus.
- Добавить empty state, upload dialog и material cards.
- Показывать queued, importing, ready и failed materials.
- Реализовать material details, archive, restore, delete и source download.
- Удалить fixture construction из production web path.

Критерий завершения: библиотека полностью строится из `/api/v1` и сохраняет
состояние после перезагрузки browser и server.

Результат:

- добавлен единый `LibraryEntry` projection для queued, importing, ready,
  failed и cancelled материалов с nullable active revision;
- `/api/v1/materials` и material details переведены на owner-scoped
  PostgreSQL application service, а fixture repository оставлен только для
  server tests;
- archive, restore и soft delete выполняются долговечно, принимают
  `Idempotency-Key` и добавляют material change/tombstone в sync log;
- Dioxus Web больше не создаёт fixture EPUB: empty/loading/error states,
  upload dialog, material cards, diagnostics, details, archive и source
  download строятся только из versioned API;
- Playwright проверяет реальный import, failed state, сведения, download,
  archive/restore, delete, browser reload, mobile viewport и повторный login;
- локальная проверка и lifecycle semantics описаны в
  [`../runbooks/api-backed-library.md`](../runbooks/api-backed-library.md).

### Этап 4. Рабочий reader — выполнен

- Добавить material reader route и загрузку `ReadingDocument`.
- Реализовать render plan и Dioxus/DOM platform adapter.
- Добавить TOC, internal links, footnotes и navigation history.
- Реализовать page-like navigation и browser-measured `PageMap`.
- Добавить lazy rendering и кеширование page map.
- Реализовать reader settings и восстановление последней позиции.
- Обеспечить desktop и mobile reader layouts.

Критерий завершения: реальный EPUB читается от начала до конца через shared
reader model без рендера исходного EPUB XHTML/CSS как product model.

Результат:

- shared core получил `RenderPlan`, непрерывные `PageBoundary`/`PageMap`,
  validation и platform-neutral navigation history;
- EPUB importer `s1.2` сохраняет typed internal links и footnote targets после
  sanitization, не передавая исходный XHTML/CSS в reader;
- Dioxus/DOM adapter измеряет скрытый page container browser layout engine,
  делит длинный текст binary search через `Range`, кеширует layout key и держит
  в активном reader DOM только текущую страницу;
- hash-route материала загружает реальный `ReadingDocument`, показывает TOC,
  internal links, footnotes, history, resources и desktop/mobile layouts;
- account-wide settings и source-backed material progress сохраняются в
  PostgreSQL с idempotency и sync changes, а page number остаётся локальным
  derived value согласно ADR 0008;
- Playwright проверяет чтение длинного реального EPUB, pagination, TOC, links,
  footnote, history, night theme, reload persistence и mobile viewport;
- локальный workflow описан в
  [`../runbooks/working-reader.md`](../runbooks/working-reader.md).

### Этап 5. Annotations и progress — выполнен

- Реализовать selection-to-anchor flow.
- Добавить создание, редактирование и удаление highlight и note.
- Добавить overlay rendering и переход из notes panel к anchor.
- Добавить optimistic UI, server acknowledgement и conflict handling.
- Сохранять reading progress и восстанавливать последнюю позицию.
- Реализовать portable annotation export.

Критерий завершения: position, highlights и notes переживают закрытие браузера
и рестарт сервера, а изменение reader settings не ломает привязки.

Результат:

- browser Selection/Range adapter мапит только source-text DOM spans в
  multi-block anchor с Unicode scalar offsets, quote/context, hashes и typed
  start/end source locators;
- PostgreSQL annotation service выполняет owner-scoped idempotent CRUD,
  optimistic revision checks, tombstones и append-only sync changes в одной
  transaction;
- Dioxus reader показывает optimistic highlights/notes, durable ack/error,
  conflict draft, overlays и mobile notes panel с переходом к anchor;
- progress восстанавливается по точному path+offset после repagination, а
  settings/progress writes сериализуются и имеют честный save state;
- portable JSON export содержит provenance, timestamps, quote/body и полный
  anchor; compatibility/recovery решение закреплено в
  [`../adr/0009-source-backed-anchor-v2.md`](../adr/0009-source-backed-anchor-v2.md);
- локальная проверка описана в
  [`../runbooks/durable-annotations.md`](../runbooks/durable-annotations.md).

### Этап 6. Baseline-источники Web и Telegram — выполнен

Обобщить уже работающий EPUB import path и добавить два узких входа,
не создавая отдельных library или reader models:

```text
EPUB upload ---------> source adapter --\
Public web URL ------> source adapter ----> durable ImportJob
Telegram text/link --> source adapter --/   -> DocumentRevision
                                           -> Normalized Content Package
                                           -> ReadingDocument
```

- Обобщить source kinds/refs, importer dispatch, job stages и persistence
  publication path; расширить source locator contract через ADR и migration.
- Добавить API и UI для импорта public HTTP/HTTPS URL.
- Реализовать bounded raw fetch → baseline `RenderedPageSnapshot` → generic
  semantic extractor → common normalized package.
- Добавить SSRF/redirect/DNS/size/timeout policy и fixture-backed extraction tests.
- Добавить account-scoped one-time Telegram pairing, identity/unlink и duplicate
  update protection.
- Добавить transport-neutral Telegram handler и local long-polling runner для
  `/start`, `/help`, `/unlink`, direct/forwarded text и ordinary web links.
- Показывать web/Telegram materials в той же API-backed library и открывать
  их тем же reader route.
- Покрыть Playwright URL journey и API/domain tests на Telegram update fixtures.

Критерий завершения: публичная server-rendered статья сохраняется по URL
и читается в общем reader; привязанный Telegram user отправляет или
пересылает текст и получает один durable material; повтор того же update не
создает дубль. Оба source path поддерживают те же progress и annotations.

Результат:

- EPUB, Web и Telegram проходят общий durable publication path с typed source
  refs, stages, locators, worker claim/lease и immutable raw source/snapshot;
- bounded Web fetch повторно проверяет DNS и redirects, pin-ит соединение,
  ограничивает response/extraction/package и покрыт committed fixtures;
- one-time Telegram pairing, unlink и update outcome атомарны; direct/forwarded
  text и одиночная URL используют общий inbox без duplicate material;
- API-backed library различает форматы, unified add flow принимает EPUB/URL,
  Telegram UI показывает loading/pairing/connected/expired states, а общий
  reader безопасно открывает external links;
- migration, compatibility и local workflow закреплены в
  [`../adr/0010-web-telegram-source-baseline.md`](../adr/0010-web-telegram-source-baseline.md)
  и [`../runbooks/web-telegram-baseline.md`](../runbooks/web-telegram-baseline.md).

### Этап 7. Сведение с UI/UX-прототипом — выполнен

- Перенести visual tokens, typography, spacing, cards, dialogs и reader chrome.
- Подключить реальные save/sync indicators.
- Добавить mobile panels и bottom sheet behavior.
- Свести add-material flow для EPUB/URL и Telegram connection states с
  общими visual tokens, diagnostics и capability flags.
- Завершить empty, loading, error, retry и expired-session states.
- Провести визуальное и accessibility сравнение с прототипом.
- Удалить или скрыть controls отложенных подсистем.

Критерий завершения: основные desktop и mobile user journeys визуально и
поведенчески соответствуют reader-first прототипу.

Результат:

- web shell, library и reader сведены на единой paper/sage системе tokens,
  typography, spacing, cards, dialogs и reader chrome; накопившийся legacy CSS
  удалён, а stylesheet подключён через Dioxus asset pipeline;
- source-backed continue card строится best-effort по progress не более восьми
  активных ready-материалов и не блокирует выдачу library; archive/delete сразу
  очищают и пересчитывают эту ограниченную S1 client projection;
- реальные settings/progress/annotation save states имеют отдельные saving,
  saved и failed состояния, а ошибки export и импорта доступны через live
  status/diagnostics с повтором;
- contextual reader panels взаимоисключающие: на широком desktop они соседние,
  на среднем viewport становятся drawers, а на touch viewport — safe-area
  bottom sheets с scrim, overscroll containment, Escape и возвратом focus;
- EPUB и public URL объединены в доступный tabbed add flow, Telegram и URL UI
  проверяют server capabilities, а library покрывает loading, empty, error,
  retry и централизованное expired-session состояние;
- native modal dialogs, skip link, bounded heading hierarchy, global
  focus-visible, 44 px touch targets и reduced-motion правила проверены вместе
  с keyboard-only сценариями;
- Playwright покрывает полный desktop lifecycle и отдельный Pixel 7 touch flow
  с reduced motion, modal/panel focus, mobile sheet и истечением сессии.

### Этап 8. Hardening и закрытая beta

- Закрыть golden fixture, security и compatibility suites.
- Добавить полный browser E2E пользовательский путь.
- Проверить account isolation, session handling и malicious import cases.
- Если Telegram включен в beta deployment, подключить transport-neutral
  handler к webhook, проверяющему secret token; long polling оставить для
  local development.
- Проверить SSRF corpus, duplicate Telegram delivery, pairing expiry/unlink и
  provider-secret redaction.
- Проверить performance budgets на больших EPUB и библиотеках.
- Заменить ограниченную client-side continue projection на server-side library
  projection без N+1 progress reads и проверить её на большой библиотеке.
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
  -> Web/Telegram source baseline
  -> UI/UX convergence
  -> beta hardening
```

После стабилизации domain и API contracts параллельно могут развиваться:

1. auth, SQLx, repositories, jobs и blobs;
2. EPUB/web/Telegram source adapters, normalized package, reader core и anchors;
3. Dioxus library, reader adapter и перенос prototype UI;
4. fixtures, Playwright, SSRF/import security, CI и deployment.

## Completion criteria

План считается выполненным, когда:

- новый пользователь может зарегистрироваться или войти и получить своё
  изолированное cloud-backed пространство;
- настоящий поддерживаемый EPUB загружается и импортируется через durable job;
- библиотека показывает persisted materials и честные import/save states;
- плохой EPUB возвращает структурированную и понятную диагностику;
- публичный server-rendered article сохраняется по URL как immutable
  text-first snapshot и открывается общим reader;
- Telegram-аккаунт привязывается одноразовым token, direct/forwarded text
  становится durable material, а duplicate update не создает дубль;
- обычная web-ссылка из Telegram использует тот же URL import path;
- reader использует `ReadingDocument`, render plan и platform adapter;
- TOC, page navigation, links, footnotes и settings работают;
- highlights, notes и progress сохраняются после browser/server restart;
- annotations используют полный source-backed anchor, а не DOM path;
- source EPUB и portable annotations можно скачать;
- desktop и mobile flows соответствуют прототипу и доступны с клавиатуры;
- account isolation, import security и persistence покрыты tests;
- golden EPUB compatibility tests, committed web/Telegram fixtures и основные
  Playwright journeys проходят;
- staging deployment имеет migrations, readiness, logs, backups и проверяемое
  восстановление;
- выполнены `make c` и `make web-e2e` в локально поддерживаемом окружении.

После выполнения расширенного S1 дальнейшая функциональность выбирается из
[`../systems/feature-registry.md`](../systems/feature-registry.md) и порядка,
зафиксированного в [`../early-slices.md`](../early-slices.md), без расширения
этого временного плана до общего roadmap Lumi.
