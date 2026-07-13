# Веб-аккаунт и облачная реплика

Status: accepted

## Контекст

Первый клиент Lumi - web. Это ускоряет проверку продукта и позволяет опереться
на классическую cloud-backed web architecture: сервер хранит состояние
аккаунта, материалы, normalized packages, blobs, search indexes and import/job
state, а browser client работает через API.

Это решение относится именно к web target. Оно не отменяет будущие native
full-copy clients: desktop/mobile должны хранить локальные копии, local blobs,
outbox/sync and offline indexes. Веб-аккаунт дает identity, session, облачное
хранилище для web, входную точку для Telegram/import providers и bootstrap для
других клиентов. Пользователь при этом должен иметь полный доступ к своим
материалам: открыть, скачать, сохранить и экспортировать.

Базовая формулировка: **web is a cloud-backed application, while native clients
are future full-copy replicas**.

## Пользовательские сценарии

- Пользователь открывает web-версию и создает аккаунт без email/password:
  Lumi генерирует seed phrase, которую пользователь сохраняет.
- Пользователь входит на новом устройстве, вводя seed phrase. Сервер не получает
  seed phrase в открытом виде.
- Пользователь получает stable `user_id` на базе UUIDv7 или более новой
  time-ordered версии UUID.
- Пользователь может добавить nickname для социальных подписи и отображения, но
  nickname не является логином, паролем или стабильным идентификатором доступа.
- Пользователь импортирует EPUB/PDF/FB2/Markdown/web article в web. Материал
  сохраняется в облачной реплике аккаунта и затем синхронизируется на другие
  клиенты.
- Пользователь пересылает материал Telegram-боту. Backend создает материал в
  облачной реплике web-аккаунта, web-клиент видит его после обновления/sync, а
  desktop/mobile получают позже через обычную синхронизацию.
- Пользователь скачивает исходный файл, экспортирует библиотеку или удаляет
  серверную копию аккаунта.
- Пользователь добавляет desktop/mobile клиент, и тот строит локальную полную
  копию состояния из cloud account state, sync log, normalized packages and
  blobs.
- Пользователь в будущем включает private/decentralized mode на native clients:
  личная библиотека удаляется из cloud replica, а сервер остается только для
  identity, encrypted relay/key envelopes and social coordination.

## Функциональные требования

### Identity и регистрация

- Primary account id: `user_id` с UUIDv7+.
- `user_id` является стабильным доменным/ACL идентификатором. Отдельный
  непоказываемый `lookup_id` используется только для поиска `AuthIdentity` при
  входе и не заменяет `user_id` в API, sync или social contracts.
- Seed phrase генерируется клиентом и является главным пользовательским
  credential для восстановления/входа.
- Seed phrase нельзя отправлять на сервер как plaintext password.
- Из seed phrase выводятся:
  - Ed25519 signing key для challenge-response входа;
  - отдельный account lookup key для поиска auth identity при login;
  - в будущем - encryption keys для E2EE personal space, если это решение будет
    принято.
- Сервер хранит только verifier/public auth material, session records и
  account metadata.
- Если пользователь потерял seed phrase и нет дополнительного recovery flow,
  восстановить аккаунт невозможно. Это должно быть явно показано при
  регистрации.

### Session и device registration

- Каждый вход создает `WebSession` и `SyncDevice`.
- Session token хранится отдельно от seed phrase и может быть отозван.
- Device list нужен для sync status, revocation и диагностики.
- Desktop/mobile могут подключаться через seed phrase login или future device
  pairing flow из уже авторизованного клиента.
- Telegram pairing token создается только из авторизованного аккаунта и связан
  с `user_id`.

### Account profile

- `AccountProfile` хранит пользовательские display-поля:
  - nickname;
  - avatar или color/icon как future option;
  - короткую подпись/description как future option.
- Nickname нужен только для социальных функций и user-facing attribution.
- Access control, sync ownership и provider links используют `user_id`, а не
  nickname.
- Nickname может быть неуникальным или иметь отдельный display discriminator;
  это не должно влиять на login.

### Cloud-backed web state

- Для web-клиента сервер является authoritative store personal space:
  материалы, metadata, normalized packages, blobs, заметки, прогресс, KB,
  generated artifacts, jobs, search indexes, sync log and blob manifests.
- Browser storage не является authoritative replica. Его можно использовать
  только для app shell, session/UI state, thumbnails, short-lived reader cache
  and other rebuildable caches.
- Web-клиент не обязан поддерживать offline reading/search как core property.
  Позже можно добавить limited read-only PWA cache, но он не меняет source of
  truth.
- Desktop/mobile после sync могут иметь собственную полную локальную копию и не
  должны зависеть от web session.
- У пользователя всегда должны быть команды download/export для исходных файлов,
  attachments, Markdown/JSON export и переносимого bundle.

### Future private/decentralized mode

- После появления mature desktop/mobile clients пользователь должен иметь
  возможность отключить cloud replica для private vault.
- В этом режиме сервер не хранит plaintext личной библиотеки, notes,
  highlights, learning history, private AI artifacts, search indexes or source
  files.
- Сервер может хранить account record, auth verifier/public material, device
  registry, encrypted key envelopes, relay metadata, shared-room membership and
  explicitly shared/public objects.
- Seed phrase генерируется и хранится пользователем; raw seed phrase никогда не
  хранится в облаке.
- Если пользователь потерял все устройства и не сделал export/backup/recovery,
  сервер не обязан и не должен уметь восстановить private vault content.
- Web в этом режиме может работать как account/social surface, но не как
  полноценный reader private vault без явного повторного включения cloud replica
  или загрузки выбранного контента.

### Хранение файлов и blobs

- Domain model работает через content-addressed `Blob` и `BlobManifest`, а не
  через конкретный backend.
- Metadata, sync log, users, sessions, provider links и manifests хранятся в
  PostgreSQL.
- Blob storage должен быть скрыт за backend interface:
  - `postgres_blob_store` может использоваться на раннем этапе ради простоты и
    транзакционности, но с явными лимитами размера;
  - `s3_blob_store` / S3-compatible object storage является ожидаемым
    production path для больших PDF, EPUB media, images, audio и массового web
    usage;
  - local filesystem/MinIO backend полезен для разработки.
- Переход с Postgres blobs на S3 не должен менять `Material`,
  `DocumentRevision`, sync changes или reader contracts.
- Blob upload/download должны поддерживать checksum validation, resumable
  transfer where practical и deduplication by content hash.

### Import inbox

- Web account имеет server-side `ImportInbox`.
- В inbox попадают:
  - web UI uploads;
  - Telegram ingestion jobs;
  - web-reader cloud browser capture jobs;
  - browser extension rendered snapshot uploads;
  - mobile WebView capture/regeneration jobs;
  - future provider imports.
- Import worker создает обычные `Material`, `DocumentRevision`, resources и
  sync changes в personal space аккаунта.
- Для пользователя материал должен выглядеть одинаково независимо от источника:
  web upload, Telegram bot или desktop import.
- Если browser session закрыта, server-side импорт продолжает выполняться и
  результат становится доступен при следующем sync.

### Account lifecycle

- Пользователь может экспортировать все данные аккаунта.
- Пользователь может удалить аккаунт. Удаление создает server-side deletion
  workflow: revoke sessions, unlink providers, tombstone sync spaces, schedule
  blob garbage collection after retention/grace period.
- Для web-аккаунта нужны quotas и понятные ошибки: storage limit, file size
  limit, import queue limit.

## Нефункциональные требования

- **Native full-copy compatibility.** Web-аккаунт не отменяет full-copy replicas
  для desktop/mobile и будущий private/decentralized mode.
- **Portability.** Пользователь может скачать/export свою библиотеку без
  закрытого серверного формата.
- **Security.** Seed phrase не хранится и не передается как plaintext. Sessions
  отзывные, provider tokens хранятся отдельно.
- **Durability.** Cloud-backed web state должен переживать закрытие вкладки,
  перезапуск backend and offline native devices.
- **Privacy.** Nickname и social profile отделены от личной библиотеки. Импорт
  из Telegram/web не публикуется автоматически.
- **Operational simplicity.** Для ранней версии допустим простой storage path,
  но backend abstraction должен оставить место для S3-compatible хранилища.
- **Auditability.** Account events, provider links, import jobs и deletion flow
  должны иметь диагностируемые статусы.

## Модель данных

```text
WebAccount
  -> AccountProfile
  -> AuthIdentity[]
  -> WebSession[]
  -> SyncDevice[]
  -> Cloud Personal Space
  -> ImportInbox
  -> BlobStore
```

Основные сущности:

- `WebAccount` - server-side account record with stable `user_id`.
- `AuthIdentity` - verifier/public key material derived from seed phrase.
- `AccountProfile` - nickname and display metadata.
- `WebSession` - active browser session.
- `SyncDevice` - зарегистрированный клиент/device: web session, desktop,
  mobile.
- `ProviderLink` - Telegram или future source provider binding.
- `ImportInbox` - server-side queue входящих материалов аккаунта.
- `ImportJob` - upload/fetch/provider processing job.
- `CloudBlobRef` - backend-specific location for content-addressed blob.
- `AccountExportJob` - durable export bundle job.
- `AccountDeletionJob` - durable deletion/purge workflow.

Предварительные формы:

```text
WebAccount {
  user_id: uuid_v7
  primary_space_id
  created_at
  status: active | suspended | deletion_pending | deleted
}

AuthIdentity {
  id
  user_id
  lookup_id
  public_key
  algorithm
  created_at
  revoked_at
}

AccountProfile {
  user_id
  nickname
  display_name
  avatar_ref
  updated_at
}

ImportJob {
  id
  user_id
  source_kind: web_upload | telegram | web_capture | extension_snapshot | mobile_capture | provider
  status: queued | processing | ready | failed | needs_user_action
  source_ref
  result_material_id
  created_at
  updated_at
}
```

## Реализация

### Registration flow

1. Web client generates seed phrase and asks the user to save it.
2. Client derives auth material from seed phrase.
3. Client requests account creation with `lookup_id` and public key; unique
   `lookup_id` prevents a second account for the same seed identity.
4. Server creates `user_id` as UUIDv7+, `WebAccount`, personal `SyncSpace` and
   first `SyncDevice`.
5. Server stores auth verifier/public material, not seed phrase.
6. Client receives session token and bootstraps from cloud account state.

Exact auth protocol принят в
[`../adr/0003-seed-derived-challenge-auth.md`](../adr/0003-seed-derived-challenge-auth.md):
24-словная BIP39 phrase кодирует 256-битную entropy, а независимые HKDF keys
используются для account lookup и Ed25519 challenge signing. Raw seed phrase и
private/derived keys не покидают client.

### Login flow

1. User enters seed phrase.
2. Client derives account lookup key/auth key.
3. Server finds `AuthIdentity` by `lookup_id` and returns challenge.
4. Client proves possession of seed-derived secret.
5. Server issues `WebSession` and registers/updates `SyncDevice`.
6. Client bootstraps from cloud account state and rebuildable browser cache.

### Storage implementation

Server modules:

- `accounts` - users, auth identities, sessions, profile.
- `sync` - spaces, devices, change log, snapshots.
- `imports` - inbox/jobs, Telegram/web upload/web URL processing.
- `blobs` - content-addressed object abstraction.
- `exports` - account export bundles.

PostgreSQL remains the system of record for relational state. Blob payloads
should go through a trait/interface so the first implementation can be simple
without making S3 migration invasive.

### Web client local state

Web local state is non-authoritative:

- app shell/service worker cache;
- session/UI state;
- short-lived reader cache;
- thumbnails or preview cache;
- explicit export/download outputs.

Web does not use SQLite WASM/OPFS or IndexedDB as a durable local Lumi vault in
the target web architecture. If browser storage is cleared, the user restores
the web app from cloud account state. If the server account is deleted and no
desktop/mobile replica/export exists, web data is gone.

## Интеграции и зависимости

- **Синхронизация.** Web account owns cloud personal space and registered
  devices. Native clients sync from/to this cloud state or, later, use private
  encrypted relay mode without cloud replica.
- **Telegram.** Bot links to `user_id`; incoming materials land in
  `ImportInbox` and become normal sync objects.
- **Web-reader.** Cloud browser capture, browser extension snapshots and mobile
  WebView capture/regeneration run through authenticated web account import
  jobs when they need server-side processing or cloud-backed storage.
- **Reader.** Web reader opens materials through API/object storage and writes
  annotations/progress/notes through server-side commands.
- **Поиск.** Web uses serious server-side search over cloud account state.
  Desktop/mobile later keep local indexes for offline/full-copy modes.
- **ИИ.** BYOK secrets for web sessions need secure storage policy. Server-side
  AI/subscription mode can use cloud replica only with explicit context policy.
- **Социальные функции.** Social ACL uses `user_id`; nickname is display-only.
- **Плагины.** Web plugins cannot assume filesystem/process access and must use
  account-scoped capabilities.

## Альтернативы

- `accepted`: web account as cloud-backed application for first web target.
- `accepted`: seed phrase as primary credential, provided raw seed never leaves
  the client.
- `rejected`: web without durable account. Это ломает Telegram ingestion,
  cross-device sync, restore после закрытия browser session и хранение файлов
  web-версии.
- `rejected`: server as sole source of truth for all future clients. Это
  конфликтует с native full-copy, exportability and future private mode.
- `rejected`: nickname/email as mandatory login. Nickname должен оставаться
  социальным display-полем, а email не должен быть обязательным для базовой
  регистрации.
- `rejected`: хранить seed phrase как password hash на сервере. Даже hash
  password-style подход слабее, чем verifier/public-key flow, и создает лишний
  риск credential reuse.
- `revisit`: Postgres-only blob storage. Может быть достаточно для раннего
  прототипа с лимитами, но production web с большими файлами почти наверняка
  потребует S3-compatible backend.
- `revisit`: E2EE personal space. Важно для приватности, но усложняет web
  search, AI, Telegram import и recovery.
- `revisit`: private/decentralized native mode. Это долгосрочная цель после
  mature desktop/mobile clients; ее нельзя обещать как свойство первого web
  target.

## Открытые вопросы

- Какие quotas нужны для web v01: общий storage, максимальный файл, дневной
  import лимит?
- Начинать ли с Postgres blob storage с жесткими лимитами или сразу подключать
  S3-compatible backend?
- Нужен ли дополнительный recovery factor кроме seed phrase?
- Должен ли nickname быть уникальным в social layer или достаточно display name
  плюс `user_id`/discriminator?
- Какая retention policy нужна после удаления аккаунта и Telegram unlink?
