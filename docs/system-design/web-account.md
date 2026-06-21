# Веб-аккаунт и облачная реплика

Status: draft

## Контекст

Первый клиент Lumi - web. Это ускоряет проверку продукта, но создает исключение
из базового local-first принципа: у web-клиента нет обычной пользовательской
папки на диске, а сервер неизбежно хранит данные аккаунта и файлы.

Это исключение не должно менять общую архитектуру на server-primary модель.
Веб-аккаунт является отдельным направлением: он дает identity, session,
облачную реплику, хранение blobs для web, входную точку для Telegram/import
providers и удобный bootstrap для других клиентов. При этом desktop/mobile
должны оставаться полноценными репликами, а пользователь должен иметь полный
доступ к своим материалам: открыть, скачать, сохранить и экспортировать.

Базовая формулировка: **web is a cloud-backed client replica, not the center of
the system**.

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
  копию состояния из облачной реплики и sync log.

## Функциональные требования

### Identity и регистрация

- Primary account id: `user_id` с UUIDv7+.
- Seed phrase генерируется клиентом и является главным пользовательским
  credential для восстановления/входа.
- Seed phrase нельзя отправлять на сервер как plaintext password.
- Из seed phrase выводятся:
  - auth key или PAKE secret для входа;
  - public/account lookup key для поиска аккаунта при login;
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

### Облачная реплика web-аккаунта

- Для web-клиента сервер хранит полную логическую копию personal space:
  материалы, metadata, заметки, прогресс, KB, generated artifacts, sync log и
  blob manifests.
- Для web-сценариев сервер также хранит blobs, которые пользователь загрузил
  через web или которые были созданы server-side import providers.
- Browser local storage/IndexedDB является локальным cache/репликой web session,
  но не единственным местом хранения web-данных.
- Desktop/mobile после sync могут иметь собственную полную локальную копию и не
  должны зависеть от web session.
- У пользователя всегда должны быть команды download/export для исходных файлов,
  attachments, Markdown/JSON export и переносимого bundle.

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
  - web-reader URL fetch jobs;
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

- **Local-first compatibility.** Web-аккаунт не отменяет full-copy replicas для
  desktop/mobile и не превращает всю систему в thin-client SaaS.
- **Portability.** Пользователь может скачать/export свою библиотеку без
  закрытого серверного формата.
- **Security.** Seed phrase не хранится и не передается как plaintext. Sessions
  отзывные, provider tokens хранятся отдельно.
- **Durability.** Облачная реплика должна переживать закрытие вкладки,
  перезапуск backend и offline devices.
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
  -> Personal SyncSpace
  -> ImportInbox
  -> BlobStore
```

Основные сущности:

- `WebAccount` - server-side account record with stable `user_id`.
- `AuthIdentity` - verifier/public key material derived from seed phrase.
- `AccountProfile` - nickname and display metadata.
- `WebSession` - active browser session.
- `SyncDevice` - зарегистрированная реплика: web session, desktop, mobile.
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
  auth_key_id
  verifier_or_public_key
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
  source_kind: web_upload | telegram | web_url | provider
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
3. Client requests account creation.
4. Server creates `user_id` as UUIDv7+, `WebAccount`, personal `SyncSpace` and
   first `SyncDevice`.
5. Server stores auth verifier/public material, not seed phrase.
6. Client receives session token and bootstraps local browser store.

Exact auth protocol is `open`: OPAQUE/PAKE or challenge signing with a key
derived from seed phrase. The accepted constraint is stronger than the exact
protocol: raw seed phrase must not leave the client.

### Login flow

1. User enters seed phrase.
2. Client derives account lookup key/auth key.
3. Server finds `AuthIdentity` by lookup key and returns challenge.
4. Client proves possession of seed-derived secret.
5. Server issues `WebSession` and registers/updates `SyncDevice`.
6. Client bootstraps from cloud replica and local browser cache.

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

Web uses browser-compatible persistence:

- IndexedDB or SQLite WASM for local object store;
- browser Cache/Blob APIs for local blob cache;
- explicit export/download actions for portable files.

If browser storage is cleared, user can restore from seed phrase and cloud
replica. If server account is deleted and no desktop/mobile replica/export
exists, web data is gone.

## Интеграции и зависимости

- **Синхронизация.** Web account owns `SyncAccount`, personal space, devices and
  cloud replica. Sync remains full-copy/server-assisted, not server-primary.
- **Telegram.** Bot links to `user_id`; incoming materials land in
  `ImportInbox` and become normal sync objects.
- **Web-reader.** URL fetch and browser capture can run through authenticated
  web account import jobs.
- **Reader.** Reader opens materials from local browser store or cloud-backed
  blobs and writes changes through the same local/outbox model.
- **Поиск.** Web can use local browser index where feasible and optional
  server-assisted index over the account cloud replica, without replacing local
  indexes on desktop/mobile.
- **ИИ.** BYOK secrets for web sessions need secure storage policy. Server-side
  AI/subscription mode can use cloud replica only with explicit context policy.
- **Социальные функции.** Social ACL uses `user_id`; nickname is display-only.
- **Плагины.** Web plugins cannot assume filesystem/process access and must use
  account-scoped capabilities.

## Альтернативы

- `accepted`: web account with cloud-backed replica as separate direction for
  `v01`.
- `accepted`: seed phrase as primary credential, provided raw seed never leaves
  the client.
- `rejected`: web without durable account. Это ломает Telegram ingestion,
  cross-device sync, restore после закрытия browser session и хранение файлов
  web-версии.
- `rejected`: server as sole source of truth for all clients. Это конфликтует с
  P2P-like/full-copy моделью и переносимостью.
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

## Открытые вопросы

- Какой exact auth protocol выбрать: OPAQUE/PAKE или challenge signing with
  seed-derived key?
- Использовать ли BIP39-compatible seed phrase или собственный wordlist/format?
- Должен ли `user_id` быть публичным UUIDv7, а auth lookup key отдельным
  непоказываемым идентификатором?
- Какие quotas нужны для web v01: общий storage, максимальный файл, дневной
  import лимит?
- Начинать ли с Postgres blob storage с жесткими лимитами или сразу подключать
  S3-compatible backend?
- Нужен ли дополнительный recovery factor кроме seed phrase?
- Должен ли nickname быть уникальным в social layer или достаточно display name
  плюс `user_id`/discriminator?
- Какая retention policy нужна после удаления аккаунта и Telegram unlink?
