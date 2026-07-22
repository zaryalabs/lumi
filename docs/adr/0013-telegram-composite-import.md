# ADR 0013: Составной Telegram source и durable media groups

Status: accepted

## Контекст

Текстовый Telegram baseline создавал отдельный материал из одного snapshot, а
одиночный URL перенаправлял в Web importer. Такой контракт не позволял сохранить
фото, несколько ссылок и исходную Telegram-вводную в одном документе. Альбомы
Telegram также приходят несколькими updates и не могут безопасно собираться
только в памяти listener-а.

## Решение

- Typed `teloxide-core` update преобразуется в transport-neutral envelope.
  Envelope содержит `bot_id`, derived `bot_scope`, message/forward provenance,
  текст или caption, Unicode-scalar entities, упорядоченные HTTP(S)-ссылки,
  descriptor наибольшего Telegram `photo`, `media_group_id` и классы
  пропущенных вложений. Bot token и configuration revision в envelope не входят.
- Обычное составное сообщение фиксируется как immutable envelope blob и
  `TelegramComposite` source ref. Source ref отдельно хранит результаты и
  безопасные статусы image/web capture; retry переиспользует готовые blobs.
  Legacy `TelegramText` source refs остаются читаемыми.
- Worker ограничивает материал десятью изображениями, 10 MiB на изображение,
  30 MiB суммарно и восемью ссылками. Не более трёх web fetch выполняются
  одновременно. Ошибка одной части создаёт diagnostic и fallback, но не
  отменяет остальные части.
- Telegram media capture получает Bot API client через late-bound registry,
  который заполняет только validated long-polling runtime. Capture сверяет
  исходный `bot_id`, вызывает `getFile`, ограничивает timeout/bytes и принимает
  только фактические JPEG, PNG или WebP. Старый listener lease не может очистить
  client новой ротации того же бота.
- Normalizer создаёт Telegram-вводную как `unit-0`, затем по unit на каждую
  ссылку. Успешные страницы сохраняют Web locators; нераскрытая ссылка получает
  fallback unit. Фото становятся `ReadingNodeKind::Image` с content-addressed
  `resource_hash`. Один composite import публикует один `DocumentRevision`.
- Media group накапливается в PostgreSQL только по точному сочетанию
  `(bot_scope, media_group_id, user_id)`. Каждый update фиксируется отдельно,
  группа закрывается после двухсекундного quiet window. Claim/lease защищает
  closure и recovery; stable idempotency key не допускает второй материал после
  crash между admission и финализацией группы.
- Видео, video note, animation/GIF, audio, voice, document/file и sticker не
  скачиваются. Их caption и ссылки всё равно импортируются. Update без текста,
  ссылок или `photo` не создаёт материал.

## Последствия

- Reader, annotations, progress и resource endpoint продолжают работать через
  общие normalized contracts; platform-specific Telegram types не выходят из
  transport adapter.
- Source download для нового материала возвращает immutable envelope, а
  сохранённые изображения и web snapshots входят в resource manifest.
- Замена токена другим bot id может привести только к частичной ошибке capture:
  текст и web-секции публикуются без изображения. Ротация токена того же bot id
  сохраняет доступ к pending file ids.
- Album update подтверждается после durable append, а не после готовности
  материала. Listener не ждёт quiet window, download или normalization.

## Совместимость и проверки

- Migration `20260722150000_telegram_composite_import.sql` добавляет durable
  accumulator и closure lease.
- Domain marker: `s1.2026-07-22.telegram-composite-v1`; normalized package
  format остаётся additive-compatible.
- Обязательны unit tests UTF-16 offsets, URL ordering/deduplication, photo size
  selection, multi-unit provenance и fallback; PostgreSQL suite проверяет
  duplicate/recovery media group и один итоговый material.

