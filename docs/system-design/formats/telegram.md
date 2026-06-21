# Telegram

Status: draft

## Контекст

Telegram-интеграция нужна как источник материалов для Lumi. Пользователь
подключает системного Telegram-бота Lumi, после чего может отправлять ему
посты, сообщения, ссылки и файлы. Lumi принимает эти входящие материалы,
нормализует их в страницы или документы и доставляет на устройства пользователя
через буфер, потому что клиентские устройства могут быть offline.

Telegram здесь не является отдельной читалкой. Это source ingestion channel:
бот принимает данные, backend создает `Material` / `ReadingDocument`, а reader
отображает результат через общий reader contract.

Новая базовая привязка: Telegram импорт работает через web-аккаунт Lumi. Бот
связывает Telegram identity с `user_id`, складывает входящие материалы в
server-side import inbox аккаунта, а затем обычная синхронизация доставляет их
в web, desktop и mobile клиенты.

## Пользовательские сценарии

- Пользователь создает или открывает web-аккаунт Lumi и привязывает к нему
  Telegram-аккаунт через pairing token.
- Пользователь пересылает боту пост из канала, сообщение из чата или несколько
  сообщений подряд.
- Пользователь отправляет ссылку на публичный `t.me` пост.
- Пользователь отправляет текст напрямую в бот.
- Пользователь отправляет файл в бот: EPUB, FB2, PDF, Markdown или другой
  поддерживаемый формат.
- Lumi подтверждает прием материала и сообщает статус: принято, обрабатывается,
  готово, нужна ручная проверка или ошибка.
- Если устройства пользователя offline, материал сохраняется в server-side
  буфере и будет доставлен при следующей синхронизации клиента.
- Если открыт только web-клиент, материал появляется в облачной реплике
  аккаунта и становится видимым после обновления/sync web session.
- Пользователь открывает полученный материал в Lumi как обычную страницу с
  заметками, хайлайтами, поиском, ИИ-действиями и обучающими механиками.

## Функциональные требования

### Привязка пользователя

- Lumi создает системного Telegram-бота.
- Пользователь привязывает Telegram через deep link вида
  `/start <pairing_token>`, созданный в приложении Lumi.
- `pairing_token` должен быть одноразовым, короткоживущим и связанным с Lumi
  `user_id` web-аккаунта.
- После `/start` backend сохраняет связь между Lumi user id и Telegram
  `user_id` / private `chat_id`.
- Бот может поддерживать fallback-сценарий: пользователь сначала пишет боту, а
  бот выдает код или ссылку, которую нужно подтвердить в авторизованной web
  session Lumi.
- Один Lumi user может привязать несколько Telegram identities только если это
  явно разрешено настройками аккаунта.
- Один Telegram identity не может быть одновременно привязан к нескольким Lumi
  users без явного re-link flow.

### Прием материалов

Бот должен принимать:

- text message;
- forwarded message;
- forwarded channel post, если Telegram передает enough origin metadata;
- public `t.me` link;
- document/file attachment;
- media message с caption, если его можно превратить в страницу;
- batches из нескольких сообщений.

Первичная нормализация:

- одиночный текст -> одна page/article;
- forwarded post -> одна page/article с source metadata;
- серия сообщений от пользователя за короткое окно -> один grouped material, если
  пользователь явно включил batch mode или бот предложил объединить сообщения;
- файл -> передается в соответствующий format importer;
- ссылка на web article -> передается в web-reader ingestion;
- public `t.me` link -> пытаемся получить post content через доступный Bot API
  context или fallback manual flow.

### Ограничения Telegram

- Бот видит только то, что Telegram доставил ему через Bot API. Он не является
  полноценным Telegram client и не должен проектироваться как crawler приватных
  чатов.
- Приватные посты и сообщения доступны только если пользователь переслал их боту
  и Telegram передал содержимое.
- Forward origin metadata может быть частично скрыта privacy-настройками
  отправителя или канала. В таком случае Lumi сохраняет материал без надежного
  author/source attribution.
- Для файлов backend получает `file_id`, затем скачивает файл через Bot API
  `getFile` / file download endpoint.
- Telegram updates могут приходить повторно; обработка должна быть idempotent по
  `update_id`, `chat_id`, `message_id` и hash payload.

### Буфер доставки

Буфер нужен потому, что конечные устройства пользователя могут быть offline.
Для web-first модели этот буфер является частью `ImportInbox` web-аккаунта:
backend может принять Telegram update, скачать файл, создать материал и
записать sync changes даже когда browser tab закрыта.

Буфер хранит:

- raw incoming Telegram update metadata;
- normalized ingestion job;
- downloaded files/resources;
- processing status;
- resulting `Material` / `DocumentRevision`;
- delivery status per user device.

Статусы:

- `received` - update принят webhook endpoint.
- `authorized` - отправитель привязан к Lumi user.
- `buffered` - job сохранен в durable queue.
- `downloading` - скачиваем файлы/resources из Telegram.
- `processing` - нормализуем в материал.
- `ready` - материал создан и доступен синхронизации.
- `delivered` - хотя бы одно устройство получило материал.
- `failed` - обработка завершилась ошибкой.
- `needs_user_action` - нужна ручная команда: объединить, выбрать формат,
  подтвердить источник, повторить скачивание.

Буфер должен быть durable, account-scoped и не зависеть от web client session.

### Команды бота

Базовые команды:

- `/start` - начало и pairing.
- `/help` - краткое описание поддерживаемых действий.
- `/status` - последние материалы и их состояние.
- `/devices` - состояние доставки по устройствам, если это не раскрывает лишние
  данные.
- `/unlink` - отвязать Telegram identity.
- `/batch` - включить режим сбора нескольких сообщений в один материал.
- `/done` - завершить текущий batch.
- `/cancel` - отменить текущий batch или processing job.

Команды не должны быть единственным UI. Основной сценарий: пользователь просто
пересылает материал боту.

### Нормализация в ReadingDocument

Telegram content превращается в `ReadingDocument`:

- message text -> paragraphs и inline marks из Telegram entities;
- message entities -> bold, italic, underline, strikethrough, spoiler, code,
  pre, blockquote, links, mentions, hashtags;
- forwarded post title/source -> material metadata;
- album/media group -> image/media blocks с captions;
- reply/quote context -> optional source context block;
- batch -> ordered section list по времени сообщений;
- unsupported media -> placeholder block с source metadata.

Telegram-specific metadata сохраняется отдельно от user-visible content:

- `telegram_user_id`;
- `chat_id`;
- `message_id`;
- `update_id`;
- `message_date`;
- `forward_origin`, если доступен;
- `source_chat`, если доступен;
- `source_message_id`, если доступен;
- `t.me` URL, если доступен или пользователь прислал ссылку;
- media/file ids;
- raw entities.

## Нефункциональные требования

- **Durability.** Принятый update нельзя потерять из-за offline client,
  перезапуска backend или временной ошибки importer.
- **Idempotency.** Повторная доставка update или повторная команда пользователя
  не должна создавать дубликаты материалов без явного намерения.
- **Privacy.** Telegram data сохраняется только для привязанного Lumi user.
  Личные сообщения не публикуются и не отправляются в ИИ без отдельного
  пользовательского действия.
- **Backpressure.** Массовая пересылка сообщений или больших файлов не должна
  перегружать importer и storage.
- **Observability.** У каждого ingestion job должен быть trace id, статус,
  причина ошибки и видимый пользователю результат.
- **Offline-first delivery.** Reader-клиент получает готовые материалы через
  обычную синхронизацию, а не через прямой live push от Telegram.

## Модель данных

```text
Telegram Bot API update
  -> webhook endpoint
  -> TelegramIdentity
  -> TelegramIngestionJob
  -> Account ImportInbox / TelegramBuffer
  -> source-specific normalizer/importer
  -> Material / ReadingDocument
  -> sync delivery to devices
```

Сущности:

- `TelegramIdentity` - связь Telegram user/chat с Lumi user.
- `TelegramPairingToken` - одноразовый токен привязки.
- `TelegramUpdateLog` - idempotency log по `update_id`.
- `TelegramIngestionJob` - durable job обработки входящего сообщения.
- `TelegramBufferedPayload` - raw payload, files metadata и extracted content.
- `ImportJob` - account-level import job, если Telegram job уже готов
  создавать материал в общем web-account inbox.
- `TelegramBatch` - временная группа сообщений пользователя.
- `TelegramSourceRef` - source metadata для материала и anchors.
- `TelegramDeliveryState` - состояние доставки материала на устройства.

Предварительная форма source ref:

```text
TelegramSourceRef {
  telegram_user_id
  chat_id
  message_id
  update_id
  message_date
  source_kind: direct_message | forwarded_message | channel_post | link | file
  forward_origin
  source_chat_id
  source_message_id
  public_url
  media_group_id
  file_id
  file_unique_id
}
```

## Реализация

### Прием updates

Production path:

- Используем Telegram webhook, а не long polling.
- Webhook endpoint живет в backend на Axum.
- `setWebhook` должен использовать `secret_token`; endpoint проверяет
  `X-Telegram-Bot-Api-Secret-Token`.
- Webhook handler быстро валидирует update, пишет durable record/job и отвечает
  Telegram без долгой обработки.
- Processing, скачивание файлов и нормализация выполняются async worker-ами.

Long polling может использоваться только для локальной разработки.

### Pipeline обработки

1. Получить webhook update.
2. Проверить secret token и basic payload limits.
3. Записать `TelegramUpdateLog` для idempotency.
4. Определить Telegram sender и найти `TelegramIdentity`.
5. Если identity не привязан, обработать только `/start`, pairing или fallback
   flow, который отправляет пользователя в web-аккаунт Lumi.
6. Создать `TelegramIngestionJob`.
7. Если есть file/media, скачать через Bot API `getFile`.
8. Определить content kind: text, forward, link, file, media, batch command.
9. Сохранить raw/source metadata в account-scoped import inbox/buffer.
10. Передать payload соответствующему normalizer/importer.
11. Создать `Material`, `DocumentRevision`, resources и source refs.
12. Отметить job как `ready`.
13. Сообщить пользователю в Telegram короткий результат.
14. Записать sync changes в personal space аккаунта.
15. Доставить материал на web/desktop/mobile через обычный sync.

### Выбор библиотек

- `teloxide` - основной Rust framework-кандидат для Telegram Bot API types,
  requests и update handling.
- `axum` - webhook endpoint и backend routing.
- `serde` / `serde_json` - хранение raw update payload и typed conversion.
- `sqlx` - durable storage для identities, jobs, update log и delivery state.
- `reqwest` или HTTP client, используемый `teloxide`, - скачивание файлов через
  Bot API.
- Existing format importers - обработка файлов, отправленных через Telegram:
  EPUB, FB2, PDF, Markdown и т.д.

### Буфер и синхронизация

Буфер должен быть server-side, потому что устройство может быть недоступно.

Минимальная схема:

- `accounts` / `web_accounts`;
- `account_import_jobs`;
- `telegram_identities`;
- `telegram_pairing_tokens`;
- `telegram_update_log`;
- `telegram_ingestion_jobs`;
- `telegram_buffered_payloads`;
- `telegram_delivery_states`;
- `materials`;
- `document_revisions`;
- `resources`.

Client sync получает готовые `Material` и `DocumentRevision` так же, как если бы
материал был добавлен через web UI. Для web-first сценария это означает, что
Telegram import сначала материализуется в cloud-backed реплике аккаунта, а не в
отдельном client-only ingestion path.

### Batch mode

Batch mode нужен для тредов и серий сообщений.

Варианты:

- explicit `/batch` -> пользователь отправляет несколько сообщений -> `/done`;
- automatic short-window grouping -> бот предлагает "объединить последние N
  сообщений?";
- one-message mode по умолчанию.

Решение для draft: one-message mode по умолчанию, explicit `/batch` как основной
способ объединения. Automatic grouping оставить `revisit`, чтобы не создавать
неожиданные материалы.

## Интеграции и зависимости

- **Reader.** Telegram normalizer выдает `ReadingDocument`; reader отвечает за
  отображение, anchors, заметки, поиск и timeline events.
- **Форматы.** Файлы, отправленные в Telegram, передаются в соответствующие
  format importers.
- **Web-reader.** Ссылки из Telegram могут уходить в web-reader ingestion.
- **Веб-аккаунт.** Pairing token, `user_id`, import inbox, blob storage и
  account lifecycle описаны в [`../web-account.md`](../web-account.md).
- **Синхронизация.** Telegram buffer создает server-side материалы, которые
  доставляются устройствам через обычный sync.
- **Поиск.** Telegram pages индексируются как обычные материалы.
- **ИИ.** Telegram importer не вызывает ИИ сам. Reader или background task
  может позже создать summary/questions/entities.
- **Социальные функции.** Пересланный пост остается личным материалом
  пользователя, пока пользователь явно не поделится им в social layer.
- **Плагины.** Новые Telegram content types можно добавлять через source
  normalizers или plugin blocks.

## Альтернативы

- `rejected`: доставлять Telegram posts напрямую на активное устройство без
  server-side буфера. Устройство может быть offline, а update нельзя надежно
  восстановить позже.
- `rejected`: делать Telegram bot полноценным Telegram client/crawler. Bot API
  имеет другую модель доступа; Lumi должен работать с тем, что пользователь
  отправил или переслал боту.
- `rejected`: создавать отдельный Telegram reader. Telegram content должен
  превращаться в обычные pages/materials Lumi.
- `revisit`: automatic grouping сообщений без явной команды пользователя.
  Удобно для тредов, но риск неожиданного объединения выше.
- `revisit`: Telegram Mini App для более богатой настройки импорта. Может быть
  полезно позже, но базовый сценарий должен работать через обычного бота.

## Открытые вопросы

- Нужно ли поддерживать публичные `t.me` links через отдельный Telegram client
  provider, если Bot API не дает получить контент по ссылке?
- Какие media types кроме текста/файлов/изображений должны становиться
  материалами: voice, video, audio, stickers, polls?
- Нужно ли делать automatic grouping для forwarded channel posts из одного
  треда/серии?
- Как долго хранить raw Telegram payload и скачанные файлы после успешной
  нормализации?
- Нужна ли пользователю настройка "сохранять все как отдельные страницы" vs
  "объединять по источнику/дню/каналу"?

## Источники

- [Telegram Bot API](https://core.telegram.org/bots/api)
- [`teloxide` crate](https://docs.rs/teloxide/latest/teloxide/)
