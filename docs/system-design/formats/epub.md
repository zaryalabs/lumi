# EPUB

Status: draft

## Контекст

EPUB - основной книжный формат для Lumi. Технически EPUB является zip-контейнером
с package-документом, manifest, spine, навигацией, XHTML-контентом, CSS,
изображениями и другими ресурсами. Для Lumi EPUB не должен иметь собственный
экран чтения: он импортируется в общую `ReadingDocument`, а отображается через
унифицированный reader.

Главное решение: EPUB-реализация - это importer, normalizer и resource pipeline,
а не EPUB-renderer. Reader отвечает за типографику, постраничное отображение,
anchors, заметки, поиск, обучение, ИИ-действия и социальные слои.
Общая архитектура reader и граница между custom reader и platform layout engines
описаны в [`../reader-architecture.md`](../reader-architecture.md).

## Пользовательские сценарии

- Пользователь добавляет `.epub`-файл в библиотеку.
- Lumi извлекает название, авторов, язык, описание, обложку, оглавление и
  структуру книги.
- Пользователь читает EPUB в том же визуальном стиле, что веб-статьи, FB2,
  Markdown и `lum`.
- Пользователь переходит по оглавлению, внутренним ссылкам, сноскам и обратным
  ссылкам.
- Пользователь видит изображения, таблицы, цитаты, списки, кодовые блоки и
  базовое inline-форматирование.
- Пользователь делает хайлайты, заметки, записи на полях и голосовые заметки с
  устойчивой привязкой к фрагментам EPUB.
- Пользователь ищет по книге и использует EPUB-текст как контекст для базы
  знаний, обучения и ИИ-действий.

## Функциональные требования

### Поддерживаемый EPUB

- Поддерживаем EPUB 2 и EPUB 3.
- DRM-free EPUB должен проходить через основной importer без внешних DRM
  зависимостей.
- DRM-protected EPUB является желаемой частью полной реализации. LCP и Adobe DRM
  проектируются как отдельный capability layer перед importer: сначала
  публикация открывается и проверяются права доступа, затем importer получает
  поток расшифрованных EPUB resources.
- Импорт начинается с проверки container structure: `mimetype`,
  `META-INF/container.xml`, package document, manifest и spine.
- EPUB 3 navigation document используется как основной источник оглавления.
- EPUB 2 NCX используется как fallback-источник оглавления.
- Spine является источником порядка чтения.
- Manifest является источником ресурсов: XHTML, изображения, CSS, шрифты,
  навигация, media overlays и прочие вложения.
- Metadata сохраняется в `Material` и `DocumentRevision`: title, creators,
  contributors, language, identifiers, publisher, dates, description, subjects,
  rights, cover и source-specific поля.
- Обложка извлекается как отдельный resource и используется библиотекой.

### Нормализация контента

- Каждый readable spine item превращается в один или несколько `ReadingNode`.
- XHTML/HTML не рендерится напрямую. Импортер разбирает DOM и мапит
  разрешенные элементы в общую модель reader.
- CSS EPUB не переносится в reader как внешний вид. Можно извлекать только
  семантические подсказки, если они не ломают единое отображение.
- Inline-семантика сохраняется: emphasis, strong, code, subscript, superscript,
  small caps, links и language spans.
- Блочная семантика сохраняется: headings, paragraphs, blockquotes, lists,
  tables, figures, captions, code/pre, horizontal rules, asides, footnotes и
  endnotes.
- Элементы, которым нужен отдельный runtime, переводятся в standard plugin
  placeholders: MathML/LaTeX, Mermaid-like diagrams, сложный SVG, media overlays
  и scripted interactivity.
- SVG, media overlays, code highlighting и MathML/LaTeX являются стандартными
  first-party capabilities для полноценного EPUB-пути. Они могут быть
  реализованы как стандартные плагины, но должны поставляться и тестироваться
  вместе с Lumi.
- Скрипты не выполняются и не попадают в output-модель.
- Формы, embedded iframes и произвольные interactive widgets не исполняются.
  Если их нужно поддержать, они должны быть преобразованы в first-party plugin
  block с явной политикой безопасности.

### CSS как семантические подсказки

EPUB CSS не управляет финальным видом reader, но importer может использовать
часть CSS как подсказки для выбора `ReadingNode` и inline-семантики.

Разрешенные подсказки:

- `display` / `visibility` только для определения скрытого контента, с
  сохранением import issue, если скрывается значимый текст;
- `font-style: italic`, `font-weight: bold`, `font-variant: small-caps` как
  fallback для inline emphasis, если семантических тегов нет;
- `text-transform` как подсказка, но без изменения исходного текста;
- `text-align` только для семантики verse/poem/centered dedication, если это
  подтверждается class/epub:type;
- `list-style-type` для восстановления типа списка;
- `break-before`, `break-after`, `page-break-*` как подсказки chapter/section
  boundaries, но не как источник pagination;
- `epub:type`, ARIA roles, `class`, `id`, `lang`, `dir` и micro-semantics как
  более приоритетные источники семантики, чем визуальные CSS-свойства.

Игнорируем для final rendering:

- шрифты, размеры, цвета, line-height, margins, padding, borders, shadows,
  background, floats, columns, absolute positioning и animation;
- любые CSS-правила, которые пытаются переопределить reader typography или
  layout;
- remote `@import` и remote font/image references.

### Ресурсы

- Изображения извлекаются из EPUB и сохраняются как локальные ресурсы материала.
- Относительные ссылки внутри XHTML переписываются в ссылки на локальные
  resources или internal reader targets.
- Внешние ресурсы по умолчанию блокируются. Reader может показать placeholder и
  источник ссылки, но не должен автоматически обращаться в сеть.
- Шрифты из EPUB не используются напрямую в reader. Возможна будущая настройка
  "использовать шрифт книги", но она должна быть совместима с единой моделью
  отображения.
- CSS сохраняется как source resource для отладки и возможного повторного
  импорта, но не является источником final rendering.

### Навигация

- EPUB navigation превращается в `ReadingDocument.toc`.
- Внутренние `href` превращаются в reader links с anchor target.
- Сноски и endnotes должны открываться в reader-native UI: popover, боковая
  панель или переход с возможностью вернуться назад.
- Если EPUB содержит landmarks, page-list или list of illustrations/tables, эти
  структуры сохраняются как дополнительные navigation groups.
- Страница в EPUB-reader является вычисляемой страницей общего reader, а не
  EPUB-специфичной страницей. Если EPUB содержит page-list, она сохраняется как
  reference navigation, но не заменяет reader pagination.

### Fixed-layout EPUB

Fixed-layout EPUB является исключением, схожим с PDF. Его нельзя просто
нормализовать без потери смысла: детские книги, комиксы и иллюстрированные
материалы часто завязаны на точную верстку.

Принятое решение:

- Reflowable EPUB проходит через общий `ReadingDocument`.
- Fixed-layout EPUB определяется при импорте и помечается в metadata.
- По умолчанию fixed-layout EPUB открывается в fidelity mode: page-based
  renderer сохраняет оригинальную раскладку, координаты, пропорции и порядок
  страниц.
- Поверх fidelity mode должен работать общий слой reader: прогресс, anchors,
  заметки, хайлайты, поиск по text layer, ИИ-контекст и timeline events.
- В UI нужна явная кнопка "нормализовать", которая открывает best-effort
  reflowable representation через `ReadingDocument`.
- Нормализованный режим fixed-layout EPUB может терять точную верстку, поэтому
  reader должен показывать, что это alternative reading mode, а не исходное
  отображение.
- Anchors fixed-layout должны хранить две привязки: координатную page/rect
  привязку для fidelity mode и текстовую/структурную привязку для
  normalized mode.

### DRM

Поддержка DRM желательна, но не должна смешиваться с обычным importer.
Архитектурная граница: `EpubAccessProvider` открывает публикацию, проверяет
права, расшифровывает ресурсы и отдает importer абстрактный readable container.

Подход:

- DRM-free EPUB использует `ZipEpubAccessProvider`.
- Readium LCP - основной желаемый DRM-путь, потому что он ближе к открытому
  EPUB-экосистемному стеку. Для mobile можно ориентироваться на Readium
  Kotlin/Swift toolkits и `liblcp`; для web/server/Rust нужно отдельно
  проверить доступность и лицензионные условия EDRLab.
- Adobe ACS/ADEPT - желательная, но коммерчески и технически более сложная
  интеграция. Ее нужно проектировать как optional licensed provider, а не как
  часть open-source ядра.
- DRM-provider не должен менять `ReadingDocument`, anchors или reader UI.
  Отличается только способ получения расшифрованных EPUB resources.
- Если DRM-публикацию нельзя открыть из-за лицензии, авторизации или истекших
  прав, материал остается в библиотеке со статусом `locked` и понятной ошибкой.

### Ошибки и деградация

- Ошибка в одном spine item не должна ломать импорт всей книги.
- Importer должен сохранять `ImportIssue` с severity, source path и причиной.
- Если оглавление не найдено, Lumi строит fallback TOC по headings из spine.
- Если metadata неполная, книга все равно добавляется в библиотеку.
- Если resource не поддерживается, reader показывает placeholder, а не ломает
  документ.

## Нефункциональные требования

- **Единый вид.** EPUB не должен приносить произвольную типографику и CSS в
  reader.
- **Безопасность.** EPUB - недоверенный zip+web-content. Нужно защищаться от
  path traversal, zip bombs, внешних ресурсов, script execution и HTML/SVG
  инъекций.
- **Детерминированность.** Один EPUB при одинаковой версии importer должен
  давать одинаковую `ReadingDocument` и стабильные source paths.
- **Offline-first.** После импорта книга читается без сети.
- **Производительность.** Большие книги импортируются инкрементально или с
  ленивой нормализацией spine items; reader не держит весь DOM книги в памяти.
- **Диагностируемость.** Для отладки сохраняются source map, import issues и
  ссылки на исходные EPUB paths.

## Модель данных

EPUB importer создает общие сущности reader и формат-специфичный source map.

```text
EpubArchive
  -> EpubPackage
  -> EpubManifest
  -> EpubSpine
  -> EpubNavigation
  -> EpubContentDocument[]
  -> ReadingDocument
```

Формат-специфичные сущности:

- `EpubArchive` - исходный `.epub`, checksum, размер, дата импорта.
- `EpubPackage` - OPF/package path, EPUB version, metadata, manifest, spine.
- `EpubManifestItem` - id, href, media type, properties, fallback.
- `EpubSpineItem` - idref, linear flag, reading order, source href.
- `EpubNavItem` - label, href, children, nav type: toc, landmarks, page-list.
- `EpubResource` - локальный resource id, source href, media type, checksum.
- `EpubContentMap` - связь `ReadingNode` с EPUB source path, DOM path, text
  range и optional EPUB CFI.
- `EpubImportIssue` - предупреждение или ошибка импорта.
- `EpubAccessProvider` - abstraction над DRM-free, LCP и Adobe-protected
  контейнерами.
- `EpubDrmState` - статус доступа: `none`, `lcp`, `adobe`, `locked`,
  `expired`, `unsupported`, `error`.

Привязка anchor должна использовать общую `Anchor`-модель reader и дополнительно
хранить EPUB-specific source:

```text
EpubAnchorSource {
  package_path
  spine_idref
  content_href
  dom_path
  text_offset_start
  text_offset_end
  epub_cfi
}
```

`epub_cfi` полезен для совместимости и экспорта, но не должен быть единственным
источником истины. Основная устойчивость достигается комбинацией `ReadingNode`
path, quote, prefix/suffix context, content hash и `DocumentRevision`.

Уровень CFI:

- Для глав/разделов генерируем CFI до spine item и element path.
- Для хайлайтов и заметок генерируем range CFI с character offsets внутри text
  nodes, когда importer может построить его детерминированно.
- Для изображений, SVG, media overlays и plugin blocks генерируем CFI до
  element/block target, а детализацию внутри блока хранит plugin-specific
  anchor.
- CFI сохраняется как compatibility/export field и используется при экспорте,
  deep links и возможной совместимости с другими читалками.
- Primary anchor Lumi остается общей anchor-моделью reader.

## Реализация

### Pipeline импорта

1. Принять файл, вычислить checksum и создать `Material`.
2. Выбрать `EpubAccessProvider`: DRM-free zip, LCP, Adobe или locked fallback.
3. Открыть readable EPUB-контейнер и проверить базовую EPUB-структуру.
4. Прочитать `META-INF/container.xml` и найти package document.
5. Распарсить OPF: metadata, manifest, spine, bindings, guide.
6. Найти navigation document: EPUB 3 nav или EPUB 2 NCX fallback.
7. Определить reflowable/fixed-layout mode.
8. Построить ordered reading spine и source map.
9. Извлечь cover и media resources в локальное resource-хранилище.
10. Для reflowable spine item распарсить XHTML/HTML.
11. Преобразовать DOM в `ReadingNode` с сохранением source path и DOM path.
12. Для fixed-layout создать fidelity page model и best-effort normalized
    representation.
13. Переписать links/resources во внутренние reader targets.
14. Сгенерировать EPUB CFI для разделов, blocks и text ranges, где возможно.
15. Создать `ReadingDocument`, `DocumentRevision`, TOC и import diagnostics.
16. Передать текстовые слои в поиск и будущие ИИ/learning pipelines.

### Выбор библиотек

Принятый стек для production importer:

- `zip` - чтение OCF/zip-контейнера. Используем с минимальным набором features,
  достаточным для EPUB: stored/deflate, без лишних алгоритмов и encryption по
  умолчанию. Нужны проверки размера распакованных файлов, лимитов количества
  entries и нормализации путей.
- `quick-xml` - streaming XML parser для `container.xml`, OPF, NCX и других XML
  metadata. Подходит для больших XML и не требует держать весь документ в DOM.
- `scraper` - HTML parser/query layer поверх Servo `html5ever` для XHTML/HTML
  content documents. Используется для построения нашего `ReadingNode`, а не для
  прямого рендера HTML.
- `url` - нормализация и разрешение relative href внутри package, manifest,
  spine, nav и content documents.
- `ammonia` - defense-in-depth sanitizer для случаев, где нужно показать или
  сохранить HTML fragment, который еще не преобразован в `ReadingNode`.
  Основной путь безопасности - whitelist mapping в нашу модель, а не
  последующий рендер sanitized HTML.
- `syntect` или аналогичный Rust highlighter - first-party code highlighting
  plugin для EPUB code/pre blocks. Точный выбор можно уточнить в plugin-документе,
  но EPUB importer должен выдавать code blocks с language metadata.
- Readium Kotlin/Swift Toolkit + EDRLab `liblcp` - reference path для LCP на
  mobile и источник архитектурных решений для DRM-provider. Для Rust/web нужно
  отдельное исследование доступности LCP-библиотеки и лицензий.

Отображение EPUB не использует отдельную EPUB-библиотеку как основной reader
path. Reflowable EPUB отображается общим reader contract поверх
`ReadingDocument`: web-версия через Dioxus Web/platform adapter, desktop/mobile
через соответствующие platform adapters из `reader-architecture.md`.

### Fixed-layout rendering

Fixed-layout renderer должен быть похож на PDF-path:

- page model с viewport, intrinsic dimensions и page order;
- слой изображений/XHTML/SVG, который сохраняет оригинальную раскладку;
- text layer для поиска, anchors, ИИ-контекста и экспорта заметок;
- annotation layer поверх page coordinates;
- кнопка перехода в normalized mode;
- shared source map между fidelity mode и normalized mode.

### Почему не готовый EPUB-renderer

Готовые EPUB-renderers решают другую задачу: взять EPUB и отрисовать его как
книгу. Lumi нужна другая граница: взять EPUB и превратить его в переносимую,
аннотируемую, индексируемую модель чтения.

Поэтому:

- EPUB-specific renderer не должен владеть pagination.
- EPUB-specific renderer не должен создавать собственную модель highlights.
- EPUB-specific renderer не должен исполнять HTML/CSS/JS книги напрямую.
- EPUB-specific renderer не должен обходить общий reader, search, learning и
  AI context.

### Безопасность импорта

- Запрещать zip paths с `..`, absolute paths, drive prefixes и invalid unicode.
- Ограничивать общий распакованный размер, размер одного resource и количество
  entries.
- Не исполнять scripts и inline handlers.
- Не загружать remote resources автоматически.
- Не доверять SVG как безопасному HTML. SVG либо проходит через отдельный
  sanitizer/plugin, либо показывается как безопасно изолированный resource.
- Не использовать EPUB CSS как trusted CSS в приложении.
- Не доверять DRM-провайдеру управление UI или storage. Он только открывает
  readable resources и сообщает access state.
- Логировать special properties для routing и диагностики: `scripted`,
  `remote-resources`, `mathml`, `svg`, `media-overlay`, fixed-layout metadata.

## Интеграции и зависимости

- **Reader.** EPUB выдает `ReadingDocument`; reader отвечает за paginated
  rendering, anchors, заметки, timeline events и панели.
- **Поиск.** EPUB importer передает normalized text по spine items и nodes в
  индекс текущего документа и глобальный индекс.
- **База знаний.** Заметки и хайлайты с EPUB anchors экспортируются с source:
  title, creator, chapter, quote, EPUB path и ссылкой назад в Lumi.
- **Obsidian.** Экспорт EPUB-заметок должен давать Markdown с wikilinks,
  цитатами и source metadata.
- **ИИ.** Reader передает ИИ-слою normalized text, metadata и anchor context.
  EPUB importer не вызывает ИИ сам.
- **Плагины.** MathML, LaTeX, Mermaid, сложный SVG, media overlays,
  interactivity и fixed-layout handling должны подключаться как стандартные
  first-party плагины или отдельные extension points. SVG, media overlays,
  MathML/LaTeX и code highlighting являются обязательными для EPUB. Произвольные
  interactive widgets не обязательны и требуют отдельного whitelist/security
  решения.
- **Синхронизация.** Синхронизируются `Material`, `DocumentRevision`,
  resources metadata, anchors, progress и annotations. Исходный EPUB-файл может
  синхронизироваться отдельно в зависимости от storage policy.

## Альтернативы

- `rejected`: использовать `epub.js` как основной renderer. Он умеет рендерить
  EPUB в браузере и поддерживает pagination, но завязан на собственный rendering
  pipeline и iframe/HTML content model. Это ломает унифицированный reader и
  усложняет общие anchors, learning analytics и ИИ-контекст.
- `rejected`: использовать Rust crate `epub` как production-ядро. Он удобен для
  чтения metadata/resources/spine, но имеет GPL-3.0 лицензию и слишком
  высокоуровневую модель навигации для нашего source map. Можно использовать
  только как reference/prototype при совместимой лицензии.
- `rejected`: рендерить XHTML EPUB напрямую в Dioxus через sanitized HTML.
  Даже с sanitizer это переносит внешний HTML/CSS в reader и делает anchors
  зависимыми от чужой DOM-структуры.
- `rejected`: полностью сохранять EPUB CSS как часть отображения. Это может
  повысить fidelity для некоторых книг, но конфликтует с единым видом Lumi.
- `rejected`: EPUB CFI как primary anchor. CFI полезен для совместимости, но для
  Lumi надежнее общая anchor-модель с quote/context/hash и source map.
- `accepted`: fixed-layout EPUB отображается в fidelity mode как исключение,
  схожее с PDF, с возможностью перейти в normalized mode.
- `accepted`: EPUB CFI генерируется до range-level для текстовых anchors, но
  остается compatibility/export полем.
- `accepted`: CSS используется только как ограниченный semantic hint layer, а
  не как источник final rendering.

## Открытые вопросы

- Какие лицензионные условия и platform boundaries потребуются для LCP и Adobe
  DRM, особенно для web и open-source поставки Lumi?
- Какой renderer безопаснее использовать для fixed-layout XHTML/SVG pages:
  sandboxed iframe, rasterization, собственный Dioxus renderer или отдельный
  first-party plugin runtime?

## Источники

- [W3C EPUB 3.3](https://www.w3.org/TR/epub-33/)
- [`zip` crate](https://docs.rs/zip/latest/zip/)
- [`quick-xml` crate](https://docs.rs/quick-xml/latest/quick_xml/)
- [`scraper` crate](https://docs.rs/scraper/latest/scraper/)
- [`ammonia` crate](https://docs.rs/ammonia/latest/ammonia/)
- [`epub.js`](https://github.com/futurepress/epub.js)
- [`epub` crate](https://docs.rs/epub/latest/epub/)
- [Readium Kotlin Toolkit](https://github.com/readium/kotlin-toolkit)
- [Readium Swift Toolkit](https://github.com/readium/swift-toolkit)
