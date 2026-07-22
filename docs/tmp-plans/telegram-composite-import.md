# Составной импорт материалов из Telegram

Status: `active`

## Назначение

Этот план расширяет существующий Telegram baseline до составного импорта:
одно обычное или пересланное Telegram-сообщение создаёт один материал Lumi, в
котором Telegram-контент является вводной секцией, а содержимое каждой
найденной публичной web-ссылки — отдельной последующей секцией.

План реализует решения и контракты из:

- [`../systems/formats/telegram.md`](../systems/formats/telegram.md);
- [`../systems/formats/web-reader.md`](../systems/formats/web-reader.md);
- [`../systems/normalized-content.md`](../systems/normalized-content.md);
- [`../adr/0010-web-telegram-source-baseline.md`](../adr/0010-web-telegram-source-baseline.md);
- [`../adr/0011-beta-operations-and-telegram-webhook.md`](../adr/0011-beta-operations-and-telegram-webhook.md).

Долгоживущие решения о составном source provenance, группировке Telegram
media group и статусах частично успешного импорта должны быть перенесены в
канонические документы или отдельный ADR до завершения среза.

## Scope

Поддерживаются:

- обычный и пересланный текст;
- подпись (`caption`) к сообщению;
- Telegram entities и HTTP(S)-ссылки, найденные в обычном тексте;
- Telegram `photo` с выбором наибольшего доступного размера;
- несколько изображений одного Telegram media group;
- раскрытие публичных HTTP(S)-ссылок через существующий bounded raw web fetch;
- один Telegram-пост или media group -> один `Material` и один
  `DocumentRevision`;
- одна раскрываемая ссылка -> один `ContentUnit` после Telegram-вводной.

Намеренно не поддерживаются и игнорируются:

- `video`, `video_note`, `animation` и GIF;
- `audio` и `voice`;
- `document` и любые файлы, включая изображения, отправленные как файл;
- stickers и остальные типы вложений, не являющиеся Telegram `photo`.

Если caption или текст сопровождает неподдерживаемое медиа, текст и ссылки
импортируются, а медиа пропускается. Если update содержит только
неподдерживаемое медиа, материал не создаётся. Вводная сохраняет исходный текст
без удаления раскрываемых URL.

## Целевой пользовательский результат

```text
Материал Lumi
  Секция 0: Telegram
    изображение 1
    изображение N
    текст или caption
  Секция 1: заголовок первой web-страницы
    извлечённые блоки первой ссылки
  Секция 2: заголовок второй web-страницы
    извлечённые блоки второй ссылки
```

Неудача отдельной web-ссылки или изображения не должна уничтожать успешно
полученные части. Для нераскрытой ссылки создаётся секция с исходным URL и
безопасной диагностикой. Неудачное изображение пропускается с диагностикой.

## Архитектурная граница

Axum webhook выполняет только ограниченную синхронную работу:

1. проверяет secret, размер и базовую форму update;
2. фиксирует idempotency claim;
3. сохраняет immutable Telegram envelope;
4. создаёт durable import job;
5. быстро возвращает подтверждение Telegram.

Скачивание изображений, web capture, нормализация и публикация выполняются
durable worker-ом. Bot token не сохраняется в БД, source artifacts,
диагностиках или логах.

## Этап 1. Расширить Telegram-контракты

В `crates/lumi-core/src/sources.rs` заменить узкий текстовый snapshot
составным Telegram envelope. Он должен хранить:

- существующие `update_id`, sender/chat/message id, дату и forward provenance;
- текст или caption;
- нормализованное представление поддерживаемых entities;
- упорядоченный список найденных ссылок;
- descriptors изображений: `file_id`, `file_unique_id`, width, height и
  известный размер;
- optional `media_group_id`.

В `crates/lumi-server/src/telegram.rs`:

- разобрать `caption`, `entities`, `caption_entities`, `photo` и
  `media_group_id`;
- выбрать photo-вариант с наибольшим разрешением;
- корректно преобразовать UTF-16 offsets Telegram entities в границы Rust
  string/Unicode scalar values;
- найти явные HTTP(S)-ссылки в тексте, объединить их с entity URLs, удалить
  точные дубликаты и сохранить порядок первого появления;
- отделить «есть неподдерживаемое вложение» от «нет импортируемого контента»;
- не отклонять весь update из-за видео, GIF, аудио или файла, если в нём есть
  импортируемый текст, caption, ссылка или photo.

Прямое сообщение, состоящее только из одного URL и не содержащее изображения
или значимого Telegram provenance, может сохранить существующий короткий путь
в `web_page`. Остальные сообщения идут через составной Telegram import.

## Этап 2. Ввести составной durable source

Расширить persisted `SourceRef` в `crates/lumi-server/src/imports.rs`, например:

```text
TelegramComposite {
  message_blob_hash
  image_blobs[]
  web_snapshots[]
  device_id
}
```

`message_blob_hash` указывает на принятый immutable envelope. Результаты
скачивания сохраняются как отдельные content-addressed blobs, а их hashes и
статусы записываются в durable source ref до следующей стадии. Retry должен
переиспользовать уже сохранённые изображения и web snapshots.

Необходимо сохранить обратную совместимость с существующим
`telegram_text` source ref и уже опубликованными материалами.

## Этап 3. Реализовать загрузку Telegram-изображений

Добавить тестируемую границу `TelegramMediaCapture`:

1. вызвать Bot API `getFile`;
2. скачать файл по выданному пути;
3. применить timeout и ограничение размера;
4. проверить фактический content type;
5. принять только JPEG, PNG или WebP;
6. сохранить bytes в blob store по content hash;
7. вернуть метаданные ресурса без bot token и полного download URL.

Production-реализация получает bot token только из runtime secret
configuration. Unit и integration tests используют fake capture.

Начальные бюджеты среза:

- не более 10 изображений;
- не более 10 MiB на изображение;
- не более 30 MiB изображений на материал;
- не более 8 раскрываемых ссылок;
- не более 3 параллельных web fetch внутри одного Telegram job.

Превышение лимита одной части не должно отменять остальные части материала.
Все отбрасывания фиксируются redacted import diagnostics.

## Этап 4. Раскрыть ссылки в durable worker

Расширить `run_telegram`:

1. прочитать сохранённый Telegram envelope;
2. скачать отсутствующие разрешённые изображения;
3. запустить существующий `BoundedWebFetcher` для каждой уникальной ссылки;
4. сохранять каждый успешный web snapshot сразу;
5. ограничить параллелизм, сохраняя пользовательский порядок секций;
6. передать envelope, image resources, snapshots и diagnostics в чистый
   составной нормализатор.

Каждая ссылка обрабатывается независимо. Ошибка DNS, SSRF policy, timeout,
неподдерживаемый content type или отсутствие извлекаемого текста создаёт
локальную диагностику и fallback-секцию, но не переводит весь job в `failed`.

## Этап 5. Построить составной Normalized Content Package

В `lumi-core` добавить чистую функцию наподобие
`import_telegram_composite`. Она должна:

- создать Telegram-вводную как первый `ContentUnit`;
- добавить image blocks с `ReadingNodeKind::Image` и `resource_hash`;
- добавить caption/text paragraphs с Telegram source locators;
- преобразовать каждый успешный web snapshot в отдельный `ContentUnit`;
- сохранить Web source locators у блоков раскрытой страницы;
- добавить fallback unit для каждой нераскрытой ссылки;
- построить navigation по всем секциям;
- добавить изображения и retained source artifacts в blob manifest;
- вычислить один normalized hash и опубликовать один `DocumentRevision`.

Текущий `build_publication`, жёстко создающий `unit-0`, следует обобщить до
нескольких units. При переносе web blocks необходимо заново сформировать
уникальные ids, `node_path`, navigation targets и internal link targets, не
теряя source locators.

## Этап 6. Поддержать media group

Telegram-альбом приходит несколькими updates с общим `media_group_id`.
Добавить durable accumulator:

- группировать только updates с точно совпадающим `media_group_id` и account;
- сохранять каждый update id для дедупликации;
- завершать группу после ограниченного quiet window;
- создавать один import job и один материал на группу;
- брать caption из элемента группы, где он присутствует;
- сохранять порядок изображений;
- не применять эвристическую группировку разных сообщений по времени.

Для accumulator следует добавить отдельную PostgreSQL migration и recovery
tests, чтобы перезапуск процесса не терял и не дублировал группу.

## Этап 7. Обновить ответы и capabilities

Обновить `/help` и ответы бота:

- поддерживаются текст, пересылки, изображения и публичные web-ссылки;
- видео, GIF, аудио и файлы пропускаются;
- принятый составной материал обрабатывается асинхронно;
- update только с неподдерживаемым содержимым не создаёт материал.

Не обещать пользователю готовый материал до успешной публикации job.

## Этап 8. Покрыть тестами

### Core unit tests

- текст без ссылок;
- caption + photo + ссылка;
- несколько ссылок с сохранением порядка;
- entity URL и plain-text URL;
- UTF-16 offsets с emoji и нелатинским текстом;
- дедупликация ссылок;
- правильный порядок units и navigation;
- Telegram/Web source locators внутри одного документа;
- image resource hash и manifest entry;
- fallback unit и diagnostics для нераскрытой ссылки.

### Server integration tests

- fake Telegram `getFile`/download;
- fake web capture с частичным успехом;
- retry переиспользует сохранённые artifacts;
- ошибка изображения не ломает текст и web sections;
- видео с caption импортирует caption и игнорирует видео;
- update только с видео не создаёт job;
- duplicate update создаёт ровно один материал;
- media group после recovery создаёт ровно один материал;
- превышение image/link budgets даёт ограниченный результат и diagnostics.

### Axum и browser tests

- webhook с photo/caption проходит secret и payload boundaries;
- webhook отвечает до выполнения сетевых capture операций;
- Playwright fixture создаёт материал из Telegram caption, изображения и двух
  web-ссылок;
- reader показывает Telegram-вводную, изображение и web-секции в правильном
  порядке;
- resource endpoint отдаёт изображение с корректным content type и защитными
  headers.

## Этап 9. Документация и архитектурная фиксация

До handoff обновить:

- `docs/systems/formats/telegram.md`;
- `docs/systems/normalized-content.md`, если уточняется composite provenance;
- `docs/adr/0010-web-telegram-source-baseline.md` или новый ADR;
- `docs/runbooks/web-telegram-baseline.md`;
- capability и user-facing help-тексты.

## Порядок поставки

1. Typed Telegram envelope и parsing без изменения публикации.
2. Composite source ref и single-message job.
3. Photo download и сохранение ресурса.
4. Multi-link fetch с partial success.
5. Multi-unit normalizer и reader/browser coverage.
6. Media group accumulator и recovery.
7. Canonical docs, operational limits и полный quality gate.

Каждый промежуточный шаг должен сохранять чтение legacy Telegram materials и
не ослаблять webhook secret, SSRF, idempotency, worker lease или blob limits.

## Критерии завершения

Срез завершён, когда:

1. Обычный или пересланный Telegram-пост с caption, photo и несколькими
   HTTP(S)-ссылками создаёт один материал.
2. Reader сначала показывает Telegram-текст и изображения, затем отдельные
   web-секции в исходном порядке ссылок.
3. Видео, GIF, аудио, документы и остальные файлы не скачиваются и не
   отображаются.
4. Caption рядом с неподдерживаемым медиа всё равно импортируется.
5. Частичная ошибка web capture или image download не уничтожает успешный
   контент.
6. Duplicate updates и retries не создают дубли материалов или ресурсов.
7. Telegram media group создаёт один материал с упорядоченными изображениями.
8. Source locators, annotations, progress и resource delivery работают через
   существующие общие reader contracts.
9. `make c` проходит полностью.
10. `make web-e2e` проходит для нового пользовательского сценария при наличии
    локального browser stack.

