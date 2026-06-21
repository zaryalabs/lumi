# FB2

Status: draft

## Контекст

FB2/FictionBook - открытый XML-формат электронных книг, популярный в
русскоязычной книжной экосистеме. В отличие от PDF и fixed-layout EPUB, FB2
почти полностью описывает структуру и семантику книги, а не внешний вид. Это
делает FB2 хорошим reflowable-источником для Lumi: importer превращает XML в
`ReadingDocument`, а отображение выполняется общим reader contract из
[`../reader-architecture.md`](../reader-architecture.md).

Главное решение: FB2 не получает отдельный renderer. FB2-реализация отвечает за
разбор XML, metadata, bodies, notes, ссылок и embedded resources. Reader
отвечает за постраничность, типографику, anchors, заметки, поиск, обучение,
ИИ-действия и социальные слои.

## Пользовательские сценарии

- Пользователь добавляет `.fb2`, `.fb2.zip` или `.fbz`-файл в библиотеку.
- Lumi извлекает название, авторов, язык, жанры, аннотацию, серии, информацию о
  публикации, обложку и встроенные изображения.
- Пользователь читает FB2 в том же визуальном стиле, что reflowable EPUB,
  веб-статьи, Markdown и `lum`.
- Пользователь переходит по оглавлению, разделам, внутренним ссылкам и сноскам.
- Пользователь видит специфичные для FB2 структуры: эпиграфы, стихи, строфы,
  цитаты, подзаголовки, аннотацию, сноски и вложенные изображения.
- Пользователь делает хайлайты, заметки, записи на полях и voice notes с
  устойчивой привязкой к FB2-фрагментам.
- Пользователь ищет по книге и использует FB2-текст как контекст для базы
  знаний, обучения и ИИ-действий.

## Функциональные требования

### Поддерживаемый FB2

- Поддерживаем FB2/FictionBook 2.x как reflowable XML.
- Поддерживаем raw `.fb2`.
- Поддерживаем ZIP-compressed FB2: `.fb2.zip` и `.fbz`.
- ZIP-архив должен содержать один основной `.fb2` XML. Если найдено несколько
  кандидатов, importer выбирает основной по deterministic rule и сохраняет
  `ImportIssue`.
- FB3 не входит в этот документ. Если понадобится FB3, он должен проектироваться
  отдельно, потому что ближе к zip-контейнеру с несколькими ресурсами.
- DRM для FB2 не проектируется как стандартная capability: у FB2 нет такого же
  общего DRM path, как LCP/Adobe для EPUB. Если конкретный поставщик использует
  собственную защиту, это отдельный source/plugin provider до importer.

### Encoding и XML

- Importer должен читать XML declaration и корректно обрабатывать распространенные
  кодировки, включая UTF-8 и Windows-1251.
- Для non-UTF-8 используется `quick-xml` с encoding support и/или
  `encoding_rs`-based decoding layer.
- UTF-16 и другие не ASCII-compatible encodings допустимы только через явный
  decoding wrapper; если decoding невозможен, импорт завершается понятной
  ошибкой.
- Root element должен быть `FictionBook`, но importer должен быть терпим к
  namespace-вариациям и отсутствующим namespace declarations, если структура
  документа однозначна.
- XML не исполняется и не трактуется как HTML. Unknown tags либо мапятся в
  generic span/block с `ImportIssue`, либо игнорируются, если не содержат
  значимого текста.

### Metadata

Из `description` сохраняем:

- `title-info`: `book-title`, `author`, `translator`, `genre`, `annotation`,
  `keywords`, `date`, `coverpage`, `lang`, `src-lang`, `sequence`.
- `document-info`: id, version, date, program-used, src-url, src-ocr, history и
  author.
- `publish-info`: book-name, publisher, city, year, isbn, sequence.
- `custom-info`: сохраняется как source-specific metadata без влияния на reader.

При неполных metadata книга все равно импортируется. Минимальный fallback title
строится из filename или первого meaningful heading.

### Нормализация контента

FB2 body превращается в `ReadingDocument`.

Маппинг:

- `body` -> document body или supplemental body.
- `body[name="notes"]`, `body[name="comments"]` и похожие named bodies ->
  notes/endnotes bodies.
- `section` -> section/chapter `ReadingNode`.
- `title` -> heading group; уровень heading определяется глубиной section.
- `subtitle` -> subtitle/heading.
- `p` -> paragraph.
- `empty-line` -> semantic break/divider, а не пустой paragraph.
- `epigraph` -> epigraph/callout block.
- `cite` -> blockquote.
- `poem` -> poem block.
- `stanza` -> stanza group.
- `v` -> verse line with preserved line break semantics.
- `text-author` -> attribution.
- `date` -> date inline/block metadata depending on context.
- `image` -> figure/image node referencing decoded `binary`.
- `table`, `tr`, `th`, `td` -> table nodes, если они встречаются.
- `annotation` -> material annotation metadata; при необходимости может также
  отображаться как front matter block.

Inline mapping:

- `emphasis` -> emphasis mark.
- `strong` -> strong mark.
- `style[name]` -> semantic inline style with source-specific style name.
- `strikethrough`, если встречается, -> strikethrough mark.
- `sub`, `sup`, если встречаются, -> subscript/superscript marks.
- `a` -> internal, note или external link.

Внешний вид FB2 не переносится в reader: у FB2 почти нет layout-уровня, а
семантические элементы отображаются через настройки Lumi.

### Сноски и ссылки

- `a xlink:href="#id"` превращается в internal reader link.
- `a type="note"` и ссылки в named notes body превращаются в footnote/endnote
  relationship.
- XPath-like links, если встречаются, сохраняются как source link и пытаются
  разрешиться best-effort. На практике primary path - `#id`.
- Внешние ссылки сохраняются, но открываются через reader policy, а не
  автоматически.
- Для каждой resolved link importer сохраняет target `ReadingNode` и source
  path.

### Изображения и binary resources

- `binary` blocks декодируются через base64 и сохраняются как локальные
  resources материала.
- `content-type` используется как primary MIME hint; если он отсутствует или
  недостоверен, importer может выполнить lightweight sniffing.
- `image xlink:href="#binary-id"` связывается с decoded resource.
- `coverpage/image` становится cover resource для библиотеки.
- Поддерживаем как минимум JPEG и PNG.
- Unsupported binary resource не должен ломать документ: создается placeholder и
  `ImportIssue`.
- Для base64 resources нужны лимиты размера, количества ресурсов и общего
  decoded size.

### Оглавление

FB2 не хранит отдельный navigation file как EPUB. Оглавление строится из:

- nested `section`;
- `title` внутри `section`;
- meaningful `subtitle`, если section title отсутствует;
- fallback на первые paragraphs только в крайнем случае.

Named notes bodies не должны попадать в основное TOC как главы книги, но должны
быть доступны как linked notes/endnotes.

### Ошибки и деградация

- Некорректный XML не должен приводить к silent import: importer фиксирует
  `ImportIssue` с path/position.
- Если часть документа повреждена, importer может импортировать восстановимую
  часть, если XML parser позволяет безопасно продолжить.
- Если `binary` поврежден, текст книги все равно импортируется.
- Если notes body отсутствует, ссылки на сноски остаются unresolved links.
- Если genre/metadata неизвестны, они сохраняются как raw source value.

## Нефункциональные требования

- **Единый вид.** FB2 всегда идет через общий reflowable reader contract.
- **Безопасность.** FB2 - недоверенный XML/ZIP/base64 input. Нужны лимиты
  размера, защита от zip bombs, XML entity expansion, path traversal и
  resource exhaustion.
- **Детерминированность.** Один FB2 при одинаковой версии importer должен давать
  одинаковые `ReadingNode` ids, source paths и anchors.
- **Offline-first.** После импорта книга читается без сети.
- **Производительность.** Большие FB2 и большие embedded images требуют
  streaming parse и ленивой обработки binary resources.
- **Диагностируемость.** Importer сохраняет source path, XML path, import issues
  и raw metadata для отладки.

## Модель данных

```text
Fb2Input
  -> Fb2AccessProvider
  -> Fb2XmlDocument
  -> Fb2Description
  -> Fb2Body[]
  -> Fb2BinaryResource[]
  -> ReadingDocument
```

Формат-специфичные сущности:

- `Fb2Input` - исходный файл: `.fb2`, `.fb2.zip` или `.fbz`, checksum, размер,
  дата импорта.
- `Fb2AccessProvider` - raw XML или ZIP-compressed XML access layer.
- `Fb2Description` - нормализованные metadata из `description`.
- `Fb2Body` - main body или named supplemental body.
- `Fb2SectionMap` - связь `section/title/p` с `ReadingNode`.
- `Fb2LinkMap` - unresolved/resolved internal, note и external links.
- `Fb2BinaryResource` - decoded binary id, MIME type, checksum, local resource id.
- `Fb2ImportIssue` - warning/error с XML path и причиной.

FB2-specific anchor source:

```text
Fb2AnchorSource {
  body_name
  xml_path
  element_id
  section_path
  text_offset_start
  text_offset_end
}
```

Primary anchor остается общей anchor-моделью Lumi: `ReadingNode` path, quote,
prefix/suffix context, content hash и `DocumentRevision`. FB2 XML path нужен для
диагностики, экспорта и восстановления после повторного импорта.

## Реализация

### Pipeline импорта

1. Принять файл, вычислить checksum и создать `Material`.
2. Определить input kind: raw `.fb2`, `.fb2.zip` или `.fbz`.
3. Для ZIP открыть архив через безопасный provider и выбрать основной `.fb2`.
4. Определить XML encoding и создать decoding reader.
5. Streaming-parse XML через `quick-xml`.
6. Извлечь `description` и нормализовать metadata.
7. Построить lightweight id index для elements и links.
8. Распарсить main body и supplemental bodies.
9. Преобразовать FB2 structure в `ReadingNode`.
10. Декодировать referenced `binary` resources и связать images/cover.
11. Разрешить internal links и notes.
12. Построить TOC из sections/titles.
13. Создать `ReadingDocument`, `DocumentRevision`, source map и import issues.
14. Передать текстовые слои в поиск и будущие ИИ/learning pipelines.

### Выбор библиотек

Принятый стек:

- `quick-xml` - основной streaming XML parser. Нужен из-за больших FB2,
  embedded base64 и требования не держать весь XML в DOM.
- `encoding_rs` - decoding legacy encodings, особенно Windows-1251. Может
  использоваться напрямую или через `quick-xml` encoding feature/decoding layer.
- `zip` - чтение `.fb2.zip` и `.fbz` с теми же ограничениями безопасности, что
  для EPUB ZIP path.
- `base64` - декодирование `binary` blocks.
- `url` - нормализация external links, если нужно хранить их как URL.

Не используем отдельный FB2 renderer. UI идет через общий reader contract и
platform adapters из [`../reader-architecture.md`](../reader-architecture.md).

### Безопасность импорта

- Отключить/не поддерживать external XML entities и DTD processing.
- Ограничивать размер raw XML, decoded XML, ZIP entries, decoded binary и общий
  resource size.
- Запрещать ZIP paths с `..`, absolute paths, drive prefixes и invalid unicode.
- Не загружать external resources автоматически.
- Не доверять MIME из `content-type` без проверки.
- Не рендерить XML/HTML fragments напрямую в UI.
- Логировать malformed XML, unknown tags, unresolved links, duplicate ids,
  oversized binaries и unsupported content types.

## Интеграции и зависимости

- **Reader.** FB2 выдает `ReadingDocument`; reader отвечает за paginated
  rendering, anchors, заметки, timeline events и панели.
- **Reader architecture.** FB2 не зависит от Dioxus/WebView/native shell и не
  содержит platform-specific render state.
- **Поиск.** FB2 importer передает normalized text по sections/nodes в индекс
  текущего документа и глобальный индекс.
- **База знаний.** Заметки и хайлайты с FB2 anchors экспортируются с source:
  title, author, section, quote, XML path и ссылкой назад в Lumi.
- **Obsidian.** Экспорт FB2-заметок должен давать Markdown с wikilinks,
  цитатами и source metadata.
- **ИИ.** Reader передает ИИ-слою normalized text, metadata и anchor context.
  FB2 importer не вызывает ИИ сам.
- **Плагины.** FB2 почти не требует plugin blocks. Если встречаются tables,
  images или code-like fragments, они идут через стандартные reader/plugin
  contracts.
- **Синхронизация.** Синхронизируются `Material`, `DocumentRevision`, resources
  metadata, anchors, progress и annotations. Исходный FB2-файл может
  синхронизироваться отдельно в зависимости от storage policy.

## Альтернативы

- `rejected`: отдельный FB2 reader. Формат reflowable и семантический, поэтому
  отдельный reader только добавит расхождения в anchors, заметках и UI.
- `rejected`: преобразовывать FB2 в HTML и рендерить HTML напрямую. Это теряет
  FB2 semantics, усложняет notes/source map и повторяет ошибку format-specific
  rendering.
- `rejected`: читать весь FB2 в DOM по умолчанию. У FB2 часто большие embedded
  images в base64, поэтому DOM-подход повышает риск памяти и latency.
- `revisit`: использовать XSD validation на import path. Полная validation
  полезна для диагностики, но может быть слишком строгой к реальным книгам из
  библиотек, где часто встречаются неидеальные FB2.
- `revisit`: поддерживать FB3 в том же importer. Вероятнее, FB3 должен быть
  отдельным форматом из-за контейнера и другой модели ресурсов.

## Открытые вопросы

- Насколько строгим должен быть импорт malformed FB2: fail fast или best-effort
  recovery с предупреждениями?
- Нужно ли показывать `annotation` как front matter перед книгой или только как
  metadata в панели материала?
- Как нормализовать genre taxonomy: сохранять raw FB2 genres или мапить их в
  общую Lumi-таксономию?
- Нужна ли отдельная политика для очень больших embedded images: decode при
  импорте или lazy decode по требованию reader?
- Поддерживаем ли XPath-like FB2 links beyond `#id`, если они реально встретятся
  в пользовательских файлах?

## Источники

- [FictionBook overview](https://en.wikipedia.org/wiki/FictionBook)
- [FictionBook description](https://ru.wikipedia.org/wiki/FictionBook)
- [`quick-xml` crate](https://docs.rs/quick-xml/latest/quick_xml/)
- [`encoding_rs` crate](https://docs.rs/encoding_rs/latest/encoding_rs/)
- [`zip` crate](https://docs.rs/zip/latest/zip/)
- [`base64` crate](https://docs.rs/base64/latest/base64/)
