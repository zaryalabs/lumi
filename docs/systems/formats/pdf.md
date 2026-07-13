# PDF

Status: accepted

## Контекст

PDF - важный формат для Lumi, но он не должен проходить тот же путь, что
reflowable EPUB, FB2, веб-страницы, Markdown и `lum`.

PDF является осознанным исключением из общей модели reader. В большинстве PDF
смысл связан не только с текстом, но и с точной страницей: колонками,
формулами, таблицами, сносками, иллюстрациями, нумерацией, полями,
сканированными страницами и ссылками на конкретные места. Попытка сделать PDF
обычным `ReadingDocument` как единственный режим разрушит fidelity и даст
ложное ощущение, что документ можно читать как книгу с reflowable-версткой.

Поэтому PDF-реализация строится как page fidelity surface:

```text
PDF file
  -> PDF access/import pipeline
  -> fixed-layout Normalized Content Package
  -> PageFidelityDocument
  -> visual page layer
  -> text layer
  -> annotation overlay layer
  -> search/AI/export layer
```

Reader core, annotations, highlights, notes, progress, timeline events,
поиск, ИИ-задачи, синхронизация и социальные слои остаются общими. Отличается
только нижний слой отображения и модель anchors: вместо reflowable
`ReadingNode`-layout PDF использует страницы, координаты и извлеченный
текстовый слой.

## Пользовательские сценарии

- Пользователь добавляет `.pdf`-файл в библиотеку.
- Lumi извлекает metadata, количество страниц, размеры страниц, outline,
  page labels, внутренние ссылки, текстовый слой и обложку/thumbnail.
- Пользователь читает PDF с сохранением оригинальной верстки страниц.
- Пользователь масштабирует страницу, переключает fit-width/fit-page, меняет
  одну/две страницы на экране и продолжает чтение с последней страницы.
- Пользователь переходит по outline, page labels, внутренним ссылкам и поиску.
- Пользователь выделяет текст, делает highlight, заметку, запись на полях или
  voice note.
- Пользователь оставляет заметку к области страницы даже если PDF не содержит
  извлекаемого текста.
- Пользователь ищет по PDF, если есть text layer или OCR layer.
- Пользователь запускает ИИ-действие над выделенным текстом, страницей,
  диапазоном страниц или областью страницы.
- Пользователь открывает сканированный PDF. Lumi показывает страницы сразу, а
  поиск/ИИ по тексту становятся доступны после OCR, если пользователь или
  настройки разрешили OCR-задачу.
- Пользователь синхронизирует прогресс, заметки и highlights между
  устройствами без изменения исходного PDF-файла.

## Функциональные требования

### Поддерживаемый PDF

- Поддерживаем PDF как fixed-layout материал, а не как reflowable книгу.
- Основной путь - DRM-free и password-protected PDF, если пользователь знает
  пароль.
- PDF без пароля, но с owner restrictions, открывается только в рамках
  поведения выбранного движка. Lumi не должен проектировать обход таких
  ограничений как продуктовую возможность, но UI может показывать
  предупреждение о source restrictions.
- PDF с неизвестным паролем остается в библиотеке со статусом `locked`.
- DRM-сценарии вне стандартного PDF encryption не входят в базовую реализацию
  и проектируются отдельно как access provider.
- Tagged PDF используется как дополнительный источник структуры и reading
  order, но не является обязательным условием импорта.
- Forms, AcroForm и XFA не являются основным сценарием Lumi. Поля можно
  отображать как часть страницы, но заполнение и сохранение форм не входят в
  базовый reader contract.
- Встроенные файлы, launch actions, JavaScript, submit actions и multimedia
  actions не исполняются.
- Page labels, outline/bookmarks, links, metadata и attachments metadata
  извлекаются, если движок может сделать это безопасно.

### Fidelity-отображение

- PDF открывается в page fidelity mode.
- Оригинальная страница сохраняет пропорции, rotation, crop/media box,
  графику, изображения, таблицы и расположение текста.
- Reader не применяет к PDF свою типографику, шрифты, межстрочные интервалы и
  reflowable pagination.
- Основные режимы просмотра:
  - one page;
  - two pages/spread на широких экранах;
  - continuous vertical scroll как удобный режим навигации;
  - paginated page-turn как режим чтения;
  - fit width, fit page и ручной zoom.
- Theme reader не должен менять PDF как документ. Допустимы только
  viewer-level режимы вроде затемнения фона, инверсии/фильтра страницы или
  sepia-фильтра, если они не меняют anchors.
- Рендер страниц ленивый и виртуализированный: активная страница, соседние
  страницы, thumbnails и cache.
- Для больших страниц и высокого zoom используется tiled rendering.
- Thumbnail для библиотеки и панели страниц создается отдельно от full page
  rendering.

### Page model

Каждая PDF-страница превращается в стабильную page model:

```text
PdfPage {
  page_index
  page_label
  media_box
  crop_box
  rotation
  width_points
  height_points
  text_layer_state
  thumbnail_resource_id
  import_issues
}
```

Координатная система Lumi для overlays должна быть единой:

- origin - top-left видимой страницы после применения CropBox и rotation;
- единицы - PDF points или normalized page units, но не device pixels;
- `rect` и `quad` хранятся независимо от текущего zoom, DPR и размера окна;
- при рендере adapter переводит page coordinates в screen coordinates.

Для точности overlays полезно хранить обе формы:

```text
PageRect {
  x
  y
  width
  height
  coordinate_space: canonical_page_points
}

NormalizedPageRect {
  x0
  y0
  x1
  y1
}
```

Primary storage должен использовать canonical page coordinates. Normalized
coordinates можно хранить как fallback для восстановления после повторного
импорта или изменения движка.

### Text layer

PDF text layer нужен не для reflow, а для:

- выделения текста;
- text anchors;
- поиска внутри PDF;
- экспорта заметок и цитат;
- ИИ-контекста;
- базы знаний;
- восстановления anchors после повторного импорта.

Text layer строится по страницам:

```text
PdfTextLayer {
  page_index
  extraction_engine
  extraction_revision
  language_hints
  reading_order_confidence
  blocks: PdfTextBlock[]
}

PdfTextBlock {
  block_index
  bbox
  text
  lines: PdfTextLine[]
}

PdfTextLine {
  line_index
  bbox
  text
  spans: PdfTextSpan[]
}

PdfTextSpan {
  span_index
  text
  bbox
  quads
  font_name
  font_size
  direction
}
```

Требования:

- Text extraction не считается идеальной. PDF может хранить текст в странном
  порядке, без пробелов, с custom encoding или как изображения.
- Для каждого PDF нужно сохранять `text_layer_state`: `none`, `native`,
  `ocr`, `mixed`, `failed`.
- Для text layer хранится `reading_order_confidence`, чтобы ИИ и экспорт могли
  понимать качество контекста.
- Search index использует extracted text, но UI поиска показывает результат на
  странице через page quads.
- Text layer не управляет визуальным рендером страницы. Визуальная правда -
  rendered page layer.

### OCR

OCR нужен для сканированных PDF, но не должен быть скрытым обязательным шагом.

Подход:

- Если native text layer отсутствует или почти пустой, материал помечается как
  `ocr_candidate`.
- OCR создается как отдельная background task, а не как часть открытия reader.
- OCR может быть локальным, серверным или plugin-provided в зависимости от
  будущего документа по ИИ/плагинам.
- OCR layer хранит confidence, язык, движок, дату обработки и ссылку на
  revision страницы.
- Anchors поверх OCR имеют более низкую надежность и должны хранить page/rect
  как primary visual fallback.
- ИИ-действия по OCR-тексту должны знать, что источник - распознанный текст,
  а не native PDF text.

### Anchors and selection

PDF anchors всегда должны поддерживать координатную привязку. Текстовая
привязка добавляется, когда есть text layer.

```text
PdfAnchorSource {
  pdf_file_checksum
  page_index
  page_label
  page_revision_hash
  page_rects
  page_quads
  text_layer_revision
  text_block_range
  text_span_range
  text_char_start
  text_char_end
  quote
  prefix_context
  suffix_context
  coordinate_space
}
```

Правила:

- Для highlight по тексту сохраняем quads и quote/context.
- Для заметки к области страницы сохраняем page rects без обязательного quote.
- Для margin note сохраняем page anchor и optional nearest text block.
- Для bookmark достаточно page index/label и viewport state.
- Для search result anchor можно создавать временный anchor на text range и
  page quads.
- DOM/canvas nodes, CSS pixels и текущий zoom не являются stable anchors.
- При повторном импорте восстановление идет в порядке:
  1. тот же `DocumentRevision` и page hash;
  2. page index + matching quote/context в text layer;
  3. page label + matching quote/context;
  4. normalized rect fallback;
  5. unresolved anchor с сохранением исходных данных.

### Аннотации и заметки

- Пользовательские annotations Lumi хранятся отдельно от исходного PDF.
- По умолчанию Lumi не изменяет PDF-файл и не записывает highlights внутрь PDF.
- Визуальный overlay поверх страницы показывает highlights, notes, margin
  notes, shared comments, search matches и AI markers.
- Annotation overlay строится по `page_rects/page_quads + optional text anchor`.
- Экспорт заметок должен сохранять source metadata: название, автор, страница,
  page label, quote, rect, дата, теги и backlink в Lumi.
- Экспорт в PDF annotations возможен как отдельная команда, но не как primary
  storage. Такой экспорт может быть lossy и требует отдельного решения по
  совместимости PDF viewers.

### Навигация

- Outline PDF превращается в navigation tree reader.
- Page labels сохраняются отдельно от `page_index`, потому что в PDF могут быть
  римские цифры, обложки, вставки и нестандартная нумерация.
- Внутренние ссылки превращаются в reader navigation actions.
- Внешние ссылки открываются только после пользовательского действия.
- History reader должен работать для переходов по outline, search, links и
  page thumbnails.
- Progress для PDF хранит страницу, page label, viewport state, zoom mode и
  процент по страницам.

### Search

- Поиск внутри PDF работает по native text layer или OCR layer.
- Search result хранит page index, text range, quote и quads.
- Если text layer отсутствует, поиск показывает состояние "текст не извлечен" и
  предлагает OCR, если OCR доступен.
- Глобальный поиск использует тот же text layer, что reader search.
- Для плохого reading order search остается полезным, потому что отображается
  через quads на странице, даже если экспортируемый текст неидеален.

### ИИ-действия

Reader создает `ReaderTask`, а не вызывает ИИ напрямую.

Контекст PDF-задачи может включать:

- выделенный text range;
- page label/page index;
- соседние text blocks;
- metadata материала;
- изображение страницы или crop области, если действие требует visual context;
- OCR confidence или native text confidence;
- пользовательскую заметку.

Правила:

- Для обычных explain/summarize/questions сначала используется text layer.
- Для layout-sensitive материалов: формулы, таблицы, диаграммы, сканы,
  презентационные PDF - задача может запросить page image/crop как visual
  context, но это отдельное явное действие или настройка.
- PDF importer не вызывает ИИ сам.
- OCR, table extraction и layout analysis являются background/plugin tasks, а
  не обязанностью reader core.

## Нефункциональные требования

- **Fidelity.** Визуальное отображение PDF должно сохранять исходную страницу
  и не зависеть от типографических настроек reflowable reader.
- **Производительность.** Большие PDF требуют виртуализации, tiled rendering,
  throttling, render queue, memory budget и disk cache.
- **Offline-first.** Уже импортированный PDF, thumbnails, text layer,
  annotations и progress должны работать без сети.
- **Безопасность.** PDF - недоверенный бинарный контейнер. Рендеринг и парсинг
  должны быть изолированы настолько, насколько позволяет target platform.
- **Переносимость.** Заметки и highlights экспортируются без изменения
  исходного PDF и содержат достаточно данных для восстановления контекста.
- **Доступность.** Если есть text layer или tagged structure, reader должен
  использовать их для screen reader/search/navigation. Если текста нет, UI
  должен честно показывать ограничение.
- **Диагностируемость.** Нужно сохранять import/render issues: encrypted,
  unsupported feature, text extraction failed, OCR needed, page render failed,
  malformed PDF, huge page, suspicious action.

## Модель данных

PDF создает `Material` и `DocumentRevision`, но вместо reflowable
`ReadingDocument` основным reader-документом является `PageFidelityDocument`.

```text
Material(kind: pdf)
  -> DocumentRevision
  -> PdfDocumentSource
  -> PageFidelityDocument
  -> PdfPage[]
  -> PdfTextLayer[]
  -> PdfOutline
  -> PdfLink[]
  -> PdfRenderAsset[]
```

Формат-специфичные сущности:

- `PdfDocumentSource` - исходный файл, checksum, размер, PDF version,
  encryption state, permissions metadata, import date.
- `PdfMetadata` - title, author, subject, keywords, creator, producer,
  creation/modification dates, language hints.
- `PdfPage` - page index, page label, boxes, rotation, dimensions, page hash,
  thumbnail.
- `PdfPageLabel` - связь физической страницы и пользовательской нумерации.
- `PdfOutlineItem` - дерево outline/bookmarks, target page и optional rect.
- `PdfLink` - internal/external link, source rect, target.
- `PdfTextLayer` - extracted или OCR text layer по страницам.
- `PdfRenderAsset` - thumbnail, cached page bitmap, tile или derived resource.
- `PdfImportIssue` - предупреждения и ошибки импорта/рендера.
- `PdfSecurityState` - `none`, `password_required`, `unlocked`,
  `unsupported_drm`, `malformed`, `suspicious_actions`.
- `PdfAnchorSource` - source map PDF anchor.

Общая `Anchor`-модель reader должна поддерживать PDF через target kinds
`page_area`, `text_range`, `page`, `document` и PDF-specific source:

```text
Anchor {
  material_id
  document_revision_id
  target_kind
  quote
  prefix_context
  suffix_context
  content_hash
  page_index
  page_rects
  source: PdfAnchorSource
}
```

Для совместимости с остальным reader domain:

- `ReadingProgress` для PDF хранит page/viewport, а не вычисляемую page map.
- `Annotation`, `Highlight`, `Note`, `VoiceNote`, `Bookmark`,
  `SharedComment` и `ReaderTask` используются те же, что для остальных
  форматов.
- `DocumentSearchService` работает с `PdfTextLayer`, а не с
  `ReadingDocument` nodes.
- Timeline events используют общие события, но payload позиции содержит
  page index/label.

## Реализация

### Pipeline импорта

1. Принять файл, вычислить checksum и создать `Material(kind: pdf)`.
2. Определить PDF access state: обычный файл, password required, unlocked,
   unsupported DRM или malformed.
3. Проверить базовые лимиты: размер файла, количество страниц, размер страницы,
   embedded streams, attachments, object count и render budget.
4. Извлечь metadata, PDF version, page count, page labels, outline и links.
5. Построить `PdfPage` model: boxes, rotation, dimensions, page hash.
6. Создать thumbnails для библиотеки и панели страниц лениво или background
   задачей.
7. Извлечь native text layer по страницам: blocks, lines, spans, quads,
   confidence и reading order hints.
8. Пометить страницы без текста как `ocr_candidate`.
9. Создать `DocumentRevision`, `PageFidelityDocument`, import diagnostics и
   source map.
10. Передать text layer в document search/global search pipelines.
11. Открыть reader сразу после минимального импорта, продолжая тяжелые задачи
   в фоне.

### PDF engine abstraction

Lumi не должен писать собственный PDF renderer/parser. Нужен abstraction layer:

```text
PdfEngine {
  open(source, password) -> PdfHandle
  metadata(handle) -> PdfMetadata
  pages(handle) -> PdfPage[]
  outline(handle) -> PdfOutline
  links(handle, page) -> PdfLink[]
  text_layer(handle, page) -> PdfTextLayer
  render_page(handle, page, scale, clip) -> PdfBitmap
  render_thumbnail(handle, page, size) -> PdfBitmap
}
```

Принятый путь:

- `pdfium-render` поверх PDFium - основной Rust/native candidate для server,
  desktop, background processing, thumbnails, bitmap/tile rendering и native
  text extraction.
- PDF.js - допустимый web adapter для browser-only rendering/text layer, если
  первый web target требует полностью клиентского открытия PDF без server-side
  обработки.
- Оба пути должны выдавать одну domain-модель: `PdfPage`, `PdfTextLayer`,
  `PdfLink`, `PdfOutlineItem`, `PdfAnchorSource`.
- Reader core не зависит от PDFium, PDF.js, canvas, DOM или WebView types.

`lopdf` можно рассматривать как secondary tool для структурной инспекции,
metadata, будущего экспорта annotations или тестовых fixtures, но не как
основной renderer и не как основной text extraction engine.

### Rendering strategy

PDF renderer отвечает только за page fidelity layer:

- bitmap/canvas page layer;
- optional tile layer на высоком zoom;
- text selection layer;
- annotation overlay layer;
- link hit areas;
- search match overlay.

Поток рендера:

1. Reader вычисляет visible page window.
2. Render queue запрашивает thumbnails/full pages/tiles с приоритетом текущей
   страницы.
3. `PdfEngine` рендерит bitmap или adapter рисует canvas.
4. Platform adapter размещает visual layer в page container.
5. Text layer размещает прозрачные spans/quads только для selection/search.
6. Annotation overlay рисует Lumi highlights/notes по page coordinates.
7. При zoom/resize меняется только mapping coordinates -> screen, anchors не
   пересчитываются.

Для Web:

- heavy rendering должен идти в worker/off-main-thread path, насколько это
  возможно;
- page containers должны быть виртуализированы;
- Dioxus components не должны хранить большие bitmap buffers в reactive state;
- canvas/image resources должны освобождаться по memory budget.

### Cache

Кэшируем производные данные, а не source of truth:

- thumbnails;
- page bitmap/tile на конкретном scale bucket;
- extracted text layer;
- OCR layer;
- page hashes;
- search index chunks.

Ключ кэша:

```text
PdfRenderCacheKey {
  document_revision_id
  page_index
  crop_box_hash
  rotation
  scale_bucket
  device_pixel_ratio_bucket
  render_engine
  render_engine_version
}
```

Cache invalidation происходит при изменении `DocumentRevision`, движка рендера,
page geometry или OCR/text extraction revision.

### Безопасность

- PDF обрабатывается как недоверенный файл.
- JavaScript, launch actions, submit actions, embedded media autorun и внешние
  ресурсы отключены.
- Внешние ссылки открываются только после явного действия пользователя.
- Embedded files не открываются автоматически.
- Password хранится только если пользователь явно включил сохранение, иначе
  используется только для текущей сессии.
- Native PDF engine желательно запускать в отдельном worker/process sandbox,
  особенно на server/desktop targets.
- Для PDFium-сборки предпочтительно отключить V8/JavaScript и XFA, если
  product requirements не требуют XFA.
- Для malformed/huge PDFs нужны hard limits и timeouts на import, render,
  text extraction и OCR.
- Ошибка рендера одной страницы не должна ломать весь документ.

### Экспорт

Базовый экспорт PDF-заметок:

- Markdown/Obsidian-compatible notes с quote, page label, title, author,
  tags, user note и Lumi backlink.
- JSON sidecar для полной переносимости anchors, rects, quads и metadata.
- Shared/social export через будущие social contracts.

Экспорт обратно в PDF:

- отдельная команда, не primary storage;
- может создавать копию PDF с embedded annotations;
- должен сохранять исходный файл неизменным;
- требует отдельного compatibility test suite по PDF viewers.

## Интеграции и зависимости

- **Reader.** PDF importer выдает fixed-layout Normalized Content Package. PDF
  использует общий reader domain core, но нижний surface -
  `PageFidelityDocument`, а не reflowable `ReadingDocument`.
- **Reading screen.** Панели, notes, highlights, search, AI actions, progress и
  social overlays работают так же, но позиция задается page/viewport.
- **Reader architecture.** PDF следует правилу fixed-layout/PDF surfaces:
  visual page layer, text layer, annotation overlay layer, search/AI/export
  layer.
- **EPUB fixed-layout.** Fixed-layout EPUB должен использовать похожую модель
  page fidelity, но может иметь optional normalized mode. PDF normalized mode
  не является базовым сценарием.
- **Поиск.** Использует `PdfTextLayer` или OCR layer. Без text layer поиск
  недоступен до OCR.
- **База знаний.** PDF highlights и notes экспортируются с page/quote/rect
  source metadata и backlink.
- **Obsidian.** Экспорт заметок должен сохранять page labels и quote blocks в
  Markdown.
- **ИИ.** Reader создает задачи с text/page/crop context. OCR, visual
  reasoning, table extraction и layout analysis должны быть отдельными
  background/plugin tasks.
- **Синхронизация.** Синхронизируются `Material`, `DocumentRevision`,
  extracted metadata, progress, anchors и annotations. Исходный PDF-файл может
  синхронизироваться отдельно в зависимости от storage policy.
- **Плагины.** OCR, table extraction, math/formula recognition, PDF export,
  specialized academic layout analysis и alternative PDF engines могут быть
  extension points.

## Альтернативы

- `rejected`: конвертировать PDF в `ReadingDocument` как единственный режим.
  Это ломает исходную страницу и особенно плохо работает для научных,
  учебных, сканированных, табличных и иллюстрированных материалов.
- `rejected`: хранить PDF highlights только как text offsets. В PDF text order
  нестабилен, а многие документы не имеют нормального text layer.
- `rejected`: хранить PDF highlights только как screen pixels. Pixel positions
  зависят от zoom, DPR, viewport и движка рендера.
- `rejected`: использовать встроенный browser/native PDF viewer как reader.
  Он не дает Lumi контроля над anchors, overlays, notes, search, timeline и
  ИИ-контекстом.
- `rejected`: модифицировать исходный PDF при каждом highlight/note. Это
  рискованно для синхронизации, конфликтов, переносимости и сохранности
  исходника.
- `rejected`: писать собственный PDF renderer. PDF слишком сложен: шрифты,
  color spaces, transparency, изображения, forms, encryption, incremental
  updates, malformed files и security issues.
- `accepted`: PDF является page fidelity exception с общей моделью annotations,
  search, AI tasks, progress и sync.
- `accepted`: anchors хранят координатную привязку и текстовую привязку, когда
  text layer доступен.
- `accepted`: PDF engine скрыт за adapter/trait и не попадает в reader core.
- `revisit`: точное разделение PDFium/PDF.js по платформам нужно подтвердить
  прототипом Web/Dioxus и требованиями offline-first.
- `revisit`: поддержка XFA/forms, embedded annotations export и PDF/A
  validation требуют отдельного проектирования.

## Открытые вопросы

- Какой PDF engine будет primary для первого web target: PDF.js в browser
  worker, PDFium через server/background processing или PDFium WASM?
- Нужно ли shipping bundled PDFium binaries для desktop/mobile, или достаточно
  system/bundled optional provider?
- Какой формат sidecar выбрать для переносимого экспорта PDF anchors:
  Lumi JSON, XFDF-like mapping или оба?
- Где проходит граница между OCR как ИИ-задачей и OCR как format-processing
  задачей?
- Нужно ли поддерживать редактирование/fill/save PDF forms в Lumi, или PDF
  forms остаются read-only?
- Какой уровень PDF/A, tagged PDF и accessibility validation нужен для v01?

## Источники

- [PDFium](https://pdfium.googlesource.com/pdfium/)
- [`pdfium-render` crate](https://docs.rs/pdfium-render/latest/pdfium_render/)
- [PDF.js](https://github.com/mozilla/pdf.js)
- [`lopdf` crate](https://docs.rs/lopdf/latest/lopdf/)
- [PDF 1.7 Reference](https://opensource.adobe.com/dc-acrobat-sdk-docs/pdfstandards/PDF32000_2008.pdf)
