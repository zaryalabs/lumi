# Синхронизация

Status: draft

## Контекст

Синхронизация в Lumi нужна для трех задач:

- держать библиотеку, заметки, прогресс, базу знаний и generated artifacts
  согласованными между устройствами пользователя;
- доставлять материалы, созданные server-side источниками вроде Telegram bot;
- поддерживать совместное чтение и общие папки без превращения сервера в
  единственный источник правды.

Базовая модель для `v01`: **full-copy client replicas with server-assisted
sync**. Каждый клиент Lumi должен иметь полную логическую копию состояния,
которое доступно пользователю: материалы, metadata, заметки, generated
artifacts, knowledge base и ссылки на blobs. Сервер используется как relay,
durable mailbox, blob store и coordination layer, но пользователь не должен
терять доступ к своим данным, если конкретный клиент уже синхронизирован.

Web-версия является специальным случаем этой модели и описана отдельно в
[`web-account.md`](web-account.md). Для web сервер хранит полноценный аккаунт,
облачную реплику и файлы, потому что browser session не может быть единственным
durable storage. Это не меняет sync contract: web остается cloud-backed
клиентской репликой, а desktop/mobile по-прежнему должны получать полную
логическую копию через тот же change log, manifests и blobs.

Это не чистый P2P в смысле прямого соединения устройств. Для web, mobile, NAT,
offline devices и Telegram ingestion нужен серверный sync endpoint. Но модель
должна быть P2P-like по свойствам:

- клиенты являются полноценными репликами, а не тонкими терминалами;
- изменения создаются локально и затем распространяются;
- сервер принимает и раздает изменения, а не исполняет всю доменную логику;
- content-addressed blobs могут быть скачаны, сохранены и экспортированы;
- при появлении прямого P2P transport его можно добавить без смены доменной
  модели.

## Пользовательские сценарии

- Пользователь импортирует книгу на web. Она появляется в desktop/mobile после
  синхронизации.
- Пользователь создает web-аккаунт по seed phrase, а затем подключает desktop
  или mobile как дополнительные реплики этого же `user_id`.
- Пользователь читает offline, делает хайлайты, заметки и меняет прогресс.
  После подключения сеть доставляет изменения на другие клиенты.
- Пользователь редактирует одну заметку на двух устройствах. Lumi показывает
  merged result или понятный конфликт, не теряя ни одну версию.
- Пользователь получает материал через Telegram bot, когда все устройства
  offline. Сервер буферизует результат, а клиент забирает его позже.
- Пользователь добавляет большой PDF. Metadata и прогресс синхронизируются
  быстро, blob скачивается на клиент по storage policy, но пользователь может
  получить исходный файл.
- Пользователь создает общую папку. Личные данные остаются в personal space,
  shared comments и activity синхронизируются через shared space.
- Пользователь экспортирует свою библиотеку или vault-like данные в файлы без
  обращения к закрытому серверному формату.

## Функциональные требования

### Реплики и источники правды

- Каждый клиент хранит локальную базу состояния Lumi.
- Клиент может работать offline для чтения, заметок, прогресса, базы знаний и
  локального поиска.
- Сервер хранит durable sync log, object snapshots и blobs, нужные для доставки
  между клиентами.
- Сервер не должен быть единственным местом, где существует пользовательский
  материал после успешной синхронизации клиента.
- Для web сервер хранит cloud-backed реплику аккаунта и blobs, а локальная
  browser-копия живет в IndexedDB/browser storage/cache. При этом пользователь
  должен иметь export/download всех файлов и данных.
- Desktop может дополнительно иметь folder projection: локальные файлы,
  Obsidian vault, downloaded blobs and export bundles.
- Mobile может хранить полную логическую копию metadata и выбранные blobs
  локально с возможностью скачать missing content перед чтением.

### Sync spaces

Синхронизация делится на пространства:

- **Personal space** - личная библиотека, заметки, прогресс, reader settings,
  knowledge base, learning state, AI artifacts.
- **Shared folder space** - общие папки, membership, comments, shared
  highlights, chat/activity и material match claims.
- **System/provider space** - identities, device records, account import inbox,
  Telegram buffer/jobs, provider metadata and sync cursors.

Personal space принадлежит одному пользователю. Shared folder space имеет
несколько участников и отдельные правила доступа.

### Типы данных

Синхронизируются:

- `Material`, `DocumentRevision`, source identity and metadata;
- content-addressed resource metadata and blob manifests;
- reading progress, bookmarks, annotations, highlights, notes and voice note
  metadata;
- knowledge base notes, folders, wikilinks, backlinks indexes and attachments;
- search index invalidation metadata, но не обязательно сами индексы;
- learning items, attempts, schedules and generated exercises;
- AI tasks, AI artifacts and conversations where sync policy allows;
- shared comments, chat messages, shared highlights, activity events and
  membership state;
- plugin installation metadata and plugin-owned sync objects when allowed.

Не синхронизируются обычным plaintext sync:

- API keys, OAuth tokens and provider secrets;
- OS-specific file handles and absolute local paths;
- temporary render caches, page maps, thumbnails caches and local index shards;
- external agent credentials and local command configuration.

### Blobs и файлы

- Large binary content хранится как content-addressed blobs.
- Blob id строится из hash содержимого, size and media type.
- `Material` ссылается на `BlobManifest`, а не на platform path.
- Один blob может использоваться несколькими material revisions.
- Конкретный server-side blob backend является implementation detail:
  PostgreSQL может быть ранним backend с лимитами, S3-compatible object storage
  ожидаемый production path для больших файлов. Domain sync видит только
  `Blob`/`BlobManifest`.
- Blob может быть загружен:
  - через клиентский import;
  - через server-side source provider;
  - через shared folder claim, если это не нарушает access policy.
- Для каждого клиента хранится local blob state: missing, downloading,
  available, pinned, evicted.
- Для user-facing модели важно не то, что каждый blob всегда физически скачан,
  а то, что пользователь имеет полный доступ к содержимому: может скачать,
  открыть, сохранить и экспортировать его при наличии прав.

### Изменения и конфликты

- Все локальные изменения сначала попадают в durable outbox.
- Изменения получают monotonic local sequence, device id and hybrid logical
  clock.
- Сервер принимает changes idempotently и возвращает sync cursor.
- Клиент применяет remote changes через deterministic reducers.
- Immutable entities вроде `DocumentRevision` и blobs не редактируются, а
  получают новую revision.
- Append-only сущности вроде reading events, attempts, comments and activity
  events не конфликтуют по содержанию.
- LWW допустим для слабых preference fields: reader settings, view options,
  non-critical flags.
- Для заметок, KB Markdown документов и plugin-owned текстовых документов нужен
  conflict-safe editing path:
  - `revisit`: использовать CRDT document model;
  - `accepted for draft`: для `v01` проектировать entity revision + three-way
    merge + explicit conflict object, не теряя ни одну версию.

### Deletion и tombstones

- Удаление синхронизируется как tombstone, а не как мгновенное удаление record.
- Tombstones нужны для offline clients and conflict resolution.
- Blob garbage collection выполняется после grace period и только если blob не
  достижим ни из одной актуальной revision, shared claim или backup policy.
- Пользовательское "удалить везде" должно создавать понятный audit event.

### Server-assisted P2P

Для `v01` transport:

```text
Client local store
  -> outbox changes
  -> sync server append/validate
  -> per-space change feed
  -> remote client inbox
  -> deterministic apply
```

В будущем можно добавить прямой P2P transport:

```text
Client A outbox
  -> WebRTC/local network/direct channel
  -> Client B inbox
  -> same deterministic apply
```

Прямой P2P не должен менять object format, change format and conflict rules.

## Нефункциональные требования

- **Offline-first.** Основные сценарии чтения и записи работают без сети.
- **Durability.** Outbox и inbox не должны терять изменения при crash/reload.
- **Idempotency.** Повторная доставка одного change не меняет результат.
- **Portability.** Пользователь может экспортировать данные в открытые файлы:
  Markdown, JSON, source blobs and attachments.
- **Observability.** UI должен показывать sync status: synced, pending,
  conflicted, missing blobs, failed.
- **Privacy.** Личные данные не отправляются в shared spaces или AI без явного
  пользовательского действия или настройки.
- **Performance.** Sync должен работать incrementally: курсоры, батчи,
  compressed payloads, lazy blob fetch and resumable upload/download.
- **Schema evolution.** Change log должен переживать миграции моделей.
- **Cross-platform.** Domain sync format не зависит от IndexedDB, SQLite,
  filesystem paths, Dioxus or WebView.

## Модель данных

```text
WebAccount / SyncAccount
  -> SyncDevice[]
  -> SyncSpace[]
  -> ChangeLog
  -> BlobStore

Client
  -> LocalStore
  -> Outbox
  -> Inbox
  -> SyncCursor per space
```

Основные сущности:

- `SyncAccount` - sync-level представление учетной записи пользователя;
  web/auth/profile детали описаны в [`web-account.md`](web-account.md).
- `SyncDevice` - зарегистрированный клиент: web session, desktop, mobile.
- `SyncSpace` - personal или shared sync namespace.
- `SyncObject` - доменная сущность, синхронизируемая по id and type.
- `SyncChange` - операция или snapshot update над object.
- `ObjectRevision` - версия object после применения change.
- `SyncCursor` - позиция клиента в change feed.
- `Blob` - content-addressed binary object.
- `BlobManifest` - список blobs/resources для material revision или artifact.
- `Tombstone` - удаление object.
- `Conflict` - конфликт, требующий merge или пользовательского решения.
- `DeviceClock` - local sequence + HLC.

Предварительный формат change:

```text
SyncChange {
  id
  space_id
  object_type
  object_id
  base_revision_id
  change_kind: create | update | delete | append | blob_ref | merge
  payload
  device_id
  local_seq
  hlc
  schema_version
  idempotency_key
}
```

Примеры object types:

- `material`;
- `document_revision`;
- `resource`;
- `annotation`;
- `note`;
- `kb_note`;
- `reading_progress`;
- `learning_item`;
- `learning_attempt`;
- `ai_task`;
- `ai_artifact`;
- `shared_comment`;
- `shared_chat_message`;
- `plugin_object`.

## Реализация

### Локальное хранилище

Базовый подход:

- Rust domain model and sync reducers.
- SQLite/SQLx для desktop/mobile/server local-like storage.
- IndexedDB или browser-compatible persistence для web, скрытая за тем же
  repository contract.
- Content-addressed blob directory/cache для desktop/mobile.
- Browser storage/blob APIs for web, plus explicit download/export.

Reader, search, learning, AI and social layers пишут только в local store.
Network sync читает outbox и применяет inbox. UI не должен напрямую зависеть от
server roundtrip для локальных изменений.

### Сервер

Backend responsibilities:

- authentication/session for web account and device registration;
- device registration;
- validating change envelope and access to sync space;
- durable append to change log;
- materialized latest object snapshots for faster bootstrap;
- blob upload/download and deduplication;
- web account cloud replica and import inbox integration;
- Telegram/server-side ingestion delivery;
- shared folder membership and access enforcement;
- sync cursors and batched delta API.

Сервер не должен выполнять reader-specific logic вроде восстановления anchors,
pagination or local search ranking. Исключения: server-side ingestion, shared
folder moderation/access and optional background AI/provider tasks.

### Bootstrap

Новый клиент:

1. Авторизуется.
2. Создает `SyncDevice`.
3. Получает список доступных spaces.
4. Загружает latest snapshots + remaining change log tail.
5. Строит локальную базу.
6. Планирует загрузку blobs по policy.
7. Перестраивает локальные индексы поиска.

### Incremental sync

1. Клиент читает local outbox.
2. Отправляет changes батчем.
3. Сервер append-only принимает валидные changes and returns ack.
4. Клиент запрашивает remote changes after cursor.
5. Клиент применяет changes в deterministic order.
6. При conflicts создает local `Conflict` object and UI notification.
7. Фоновые задачи обновляют search index, graph indexes and derived caches.

### Storage policy

Нужны политики:

- `metadata_only` - metadata and user data, blobs lazy.
- `full_library` - все blobs пользователя pin/download на устройстве.
- `on_open` - blob скачивается при открытии материала.
- `manual_pin` - пользователь закрепляет отдельные материалы/folders.

Даже при lazy policy пользователь должен иметь явную команду скачать/экспортировать
исходный файл или package, если у него есть access.

## Интеграции и зависимости

- **Reader.** Reader пишет progress, annotations, bookmarks, tasks and events в
  local store. Sync доставляет эти records на другие устройства.
- **Веб-аккаунт.** Account/auth/profile, seed phrase login, import inbox and
  cloud replica описаны в [`web-account.md`](web-account.md). Sync получает от
  него `user_id`, devices, spaces and storage backend.
- **Форматы.** Importers создают immutable `DocumentRevision` and resources;
  sync распространяет их metadata and blobs.
- **База знаний.** KB Markdown documents are sync objects with text revisions
  and link indexes.
- **Obsidian.** Filesystem projection не является primary sync source; она
  читает/пишет через local store and conflict rules.
- **Поиск.** Индексы перестраиваются локально из synced state. Server-side
  search может появиться для web/shared scenarios, но не заменяет local index.
- **Learning.** Attempts and schedules должны sync-иться как user-private state.
- **Social.** Shared folders являются отдельными spaces with membership and
  material access checks.
- **ИИ.** AI tasks and artifacts sync-ятся как обычные objects. Secrets and
  provider keys не sync-ятся plaintext.
- **Плагины.** Plugin-owned objects sync-ятся только после capability grant and
  schema declaration.

## Альтернативы

- `rejected`: server as primary database and thin clients. Это конфликтует с
  переносимостью, offline-first reading and P2P-like моделью.
- `accepted`: web as cloud-backed client replica. Web требует server-side
  account/files, но это оформляется как отдельная реплика, а не как отказ от
  full-copy модели.
- `rejected`: direct P2P only without server. Web/mobile/offline/Telegram and
  shared folders требуют durable rendezvous and mailbox.
- `rejected`: синхронизировать только metadata без content access. Это ломает
  требование полного доступа пользователя к материалам и экспорту.
- `rejected`: хранить пользовательские изменения как произвольные SQL dumps.
  Это непереносимо и плохо работает с multi-client conflicts.
- `revisit`: CRDT для всех изменяемых документов. Может быть правильным для
  KB/editor layer, но усложняет `v01`; сначала нужен typed domain log and
  conflict objects.
- `revisit`: end-to-end encryption для personal space. Важно для приватности,
  но влияет на web search, AI, shared folders and server-side ingestion.

## Открытые вопросы

- Какой exact local store выбрать для web: IndexedDB напрямую, SQLite WASM или
  hybrid?
- Нужна ли E2EE для personal space в `v01`, если часть AI/search/server-side
  сценариев требует content access?
- Где проходит граница physical full copy vs logical full copy для mobile при
  больших PDF and media?
- Какой CRDT/merge strategy выбрать для KB Markdown документов, если two-way
  Obsidian editing станет важным ранним сценарием?
- Как долго хранить server-side change log до compaction snapshots?
