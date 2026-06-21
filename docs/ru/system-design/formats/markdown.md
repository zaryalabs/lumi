# Markdown

Status: accepted

## Контекст

Markdown нужен Lumi как переносимый текстовый формат для статей, заметок,
экспортов, документации и материалов, которые пользователь уже хранит в plain
text. В отличие от `lum`, обычный Markdown не является полноценным книжным
проектом с manifest, интерактивными блоками и управляемой структурой папки.
Markdown importer работает с одним документом или с одним документом плюс
локальные assets.

Главное решение: Markdown не получает отдельный renderer. Markdown парсится в
AST, нормализуется в `DocumentRevision` and Normalized Content Package, а
reader строит `ReadingDocument` через общий reader contract из
[`../reader-architecture.md`](../reader-architecture.md).
Raw HTML и platform-specific Markdown preview не должны обходить reader model,
anchors, поиск и timeline.

## Пользовательские сценарии

- Пользователь добавляет `.md` или `.markdown` файл в библиотеку.
- Lumi извлекает заголовок, metadata из front matter, структуру headings,
  локальные изображения и ссылки.
- Пользователь читает Markdown в том же визуальном стиле, что reflowable EPUB,
  FB2, веб-страницы и `lum`.
- Пользователь переходит по оглавлению, headings, internal anchors, footnotes и
  wikilinks.
- Пользователь видит таблицы, task lists, code blocks, blockquotes, callouts,
  изображения и inline formatting.
- Пользователь делает хайлайты, заметки, записи на полях и voice notes с
  устойчивой привязкой к Markdown-фрагментам.
- Пользователь импортирует Markdown, созданный в Obsidian-like среде, без потери
  wikilinks и базовых callouts.

## Функциональные требования

### Поддерживаемый Markdown

- Базовый синтаксис: CommonMark.
- Расширения: GitHub Flavored Markdown для tables, strikethrough, autolinks и
  task lists.
- Файлы: `.md`, `.markdown`.
- Encoding: UTF-8 с optional BOM. Non-UTF-8 Markdown можно поддержать позже
  через отдельный decoding layer, но primary path - UTF-8.
- Обычный Markdown импортируется как один `Material`.
- Папка Markdown-файлов не становится автоматически одной книгой. Это зона
  `lum` или Obsidian-интеграции. Markdown importer может импортировать папку как
  набор отдельных материалов, если такой source flow будет нужен.
- MDX не входит в базовую поддержку. JSX/React-like blocks должны быть
  rejected/placeholder, а не исполняться.

### Диалекты и расширения Lumi

Markdown фрагментирован, поэтому importer должен явно фиксировать dialect:

- `commonmark` - строгий baseline.
- `gfm` - default для пользовательских Markdown-файлов.
- `lumi-markdown` - GFM + first-party extensions.

First-party extensions для `lumi-markdown`:

- YAML front matter через `---`.
- TOML front matter через `+++`.
- Obsidian-style wikilinks: `[[target]]`, `[[target|alias]]`,
  `[[target#heading]]`.
- Obsidian-style embeds: `![[asset-or-note]]` как image/embed placeholder.
- Obsidian/GitHub-like callouts: `> [!note]`, `> [!warning]`, `> [!tip]`.
- Typed fenced blocks для first-party rich rendering: `mermaid`, `math`,
  `latex`, `svg`, `lumi:*` и позже явно разрешенные visualization blocks.
- Tags в front matter. Inline `#tag` можно извлекать как best-effort только вне
  code/pre/link contexts.

Эти расширения не должны превращать Markdown в `lum`. `lum` остается отдельным
форматом с manifest, интерактивными блоками и контролируемым runtime.

### Rich extensions

Lumi должен поддерживать выразительный Markdown, но через typed blocks, а не
через произвольный HTML/JS runtime.

Базовая политика:

- ` ```mermaid` -> `PluginBlock(kind = "lumi.mermaid")`.
- ` ```math` / ` ```latex` -> math/LaTeX plugin block или display math node.
- ` ```svg` -> SVG plugin/resource block через sanitizer или sandbox policy.
- ` ```lumi:<type>` -> Lumi typed block, если dialect разрешает этот тип.
- Unknown fenced languages остаются обычными code blocks.
- Unsupported known rich block создает placeholder и `MarkdownImportIssue`;
  видимый source text должен оставаться recoverable.

Plain Markdown может содержать rich blocks для чтения, но не получает book-level
capabilities сам по себе. Если блоку нужны manifest-declared plugins, local
datasets, package resources, learning state или стабильные cross-file anchors,
это должен быть `lum` project, а не одиночный Markdown-файл.

Raw HTML не является способом добавлять динамику. Даже если HTML синтаксически
валиден, importer либо мапит небольшой safe subset в `ReadingNode`, либо
сохраняет его как escaped/placeholder content. Динамическое поведение должно
жить в plugin blocks с явными capabilities, sandboxing, measurement hints и
fallback.

Если позже появится `lum-dynamic`, обычный Markdown importer все равно не должен
сам включать JS runtime. Динамическое поведение должно активироваться только через
отдельный `lum`/plugin capability profile, где пользователь явно соглашается на
риски и видит degraded guarantees.

### Front matter

Importer поддерживает front matter в начале файла:

- YAML: `--- ... ---`.
- TOML: `+++ ... +++`.

Metadata mapping:

- `title` -> `Material.title`.
- `subtitle` -> material metadata.
- `authors` / `author` -> creators.
- `date`, `created`, `updated` -> material dates.
- `tags` -> material tags.
- `source`, `url`, `canonical_url` -> source metadata.
- unknown fields -> source-specific metadata.

Front matter не отображается как часть документа по умолчанию. Если нужно
показать metadata пользователю, это делает material panel, а не reader body.

### Нормализация контента

Markdown AST нормализуется в Normalized Content Package. `ReadingDocument`
строится из него как reader-facing view.

Block mapping:

- document -> root document node.
- heading -> heading node with level and generated anchor id.
- paragraph -> paragraph.
- thematic break -> divider.
- blockquote -> blockquote или callout, если match callout syntax.
- list -> ordered/unordered list.
- task list item -> task item with checked state.
- fenced code block -> code block with language/info string.
- recognized rich fenced code block -> typed plugin block.
- indented code block -> code block without language.
- table -> table nodes.
- footnote definition -> footnote/endnote node, если extension enabled.
- image block или paragraph-only image -> figure/image.
- raw HTML block -> sanitized mapped block или unsupported placeholder.

Inline mapping:

- emphasis -> emphasis.
- strong -> strong.
- strikethrough -> strikethrough.
- code span -> inline code.
- link -> internal/external/wikilink reader link.
- image -> image node or inline image placeholder.
- autolink -> external link.
- hard break -> line break.
- soft break -> space or line break according to reader settings.
- raw HTML inline -> sanitized mapped inline или text/placeholder.

### Links

Link handling:

- Relative links to local Markdown files become internal material references
  when target is known.
- Relative links to images/resources become local resource references.
- Heading fragments map to generated heading anchors.
- Wikilinks become unresolved or resolved Lumi internal references.
- External links are stored as URLs and opened through reader policy.
- Broken links are preserved with diagnostics; importer must not drop visible
  link text.

Heading anchors:

- Generated deterministically from heading text.
- Must preserve collision handling: duplicate headings get stable suffixes.
- Source offsets and heading ids are stored in `MarkdownSourceMap`.

### Images and local resources

- Local image references are resolved relative to the Markdown file path.
- Supported image types follow common reader resource policy: JPEG, PNG, GIF,
  WebP, SVG through first-party SVG capability.
- External images are not downloaded automatically by default. Reader shows a
  placeholder or asks user/source policy to fetch.
- Missing local resources create `ImportIssue` and placeholder, not import
  failure.
- Image alt text is preserved as caption/accessibility metadata.

### Raw HTML

Markdown allows raw HTML, but Lumi must not render arbitrary HTML directly.

Policy:

- No scripts, styles, iframes, object/embed, event handlers or remote resources.
- Safe inline subset can be mapped to `ReadingNode` marks: `br`, `sub`, `sup`,
  `kbd`, `mark`, maybe `abbr`.
- Safe block subset can be mapped when semantics are clear: `details/summary`,
  simple `div`/`span` with no style, simple tables if parser exposes them.
- Everything else becomes unsupported placeholder with preserved source text or
  escaped visible text.
- `ammonia` can be used as defense-in-depth if HTML fragments ever need to be
  displayed, but primary path is AST-to-ReadingNode mapping, not sanitized HTML
  rendering.

## Нефункциональные требования

- **Единый вид.** Markdown всегда идет через общий reflowable reader contract.
- **Переносимость.** Importer должен сохранять исходный Markdown path, source
  offsets, front matter и link/resource mapping.
- **Редактируемость в будущем.** Markdown должен сохранять enough source map,
  чтобы позже добавить round-trip editing/export без потери anchors.
- **Безопасность.** Raw HTML, external images и links не должны обходить reader
  security policy.
- **Детерминированность.** Один Markdown при одинаковых settings importer должен
  давать одинаковые node ids, heading anchors и source map.
- **Offline-first.** Локальные ресурсы импортируются или связываются так, чтобы
  чтение работало без сети.

## Модель данных

```text
MarkdownFile
  -> FrontMatter
  -> MarkdownAst
  -> MarkdownSourceMap
  -> DocumentRevision
  -> Normalized Content Package
  -> ReadingDocument
```

Формат-специфичные сущности:

- `MarkdownFile` - path, checksum, size, imported_at, dialect.
- `MarkdownFrontMatter` - parsed YAML/TOML metadata and raw source.
- `MarkdownAstNodeMap` - связь parser AST node с `ReadingNode`.
- `MarkdownSourceMap` - byte/line ranges, heading anchors, link definitions,
  resource refs.
- `MarkdownResource` - local or external image/resource reference.
- `MarkdownImportIssue` - warning/error с source range.

Markdown-specific anchor source:

```text
MarkdownAnchorSource {
  file_path
  dialect
  byte_start
  byte_end
  line_start
  line_end
  heading_path
  generated_heading_id
}
```

Primary anchor остается общей anchor-моделью Lumi: `ReadingNode` path, quote,
prefix/suffix context, content hash и `DocumentRevision`.

## Реализация

### Pipeline импорта

1. Принять файл, вычислить checksum и создать `Material`.
2. Проверить encoding и normalized line endings.
3. Извлечь front matter, если он есть.
4. Определить dialect: default `gfm`, explicit metadata может выбрать
   `commonmark` или `lumi-markdown`.
5. Распарсить Markdown в AST.
6. Построить heading anchors и TOC.
7. Преобразовать AST в `ReadingNode`.
8. Разрешить links, wikilinks и local resources.
9. Преобразовать recognized rich fenced blocks в typed plugin blocks.
10. Создать placeholders/import issues для raw HTML, missing resources и
   unsupported syntax.
11. Создать `DocumentRevision`, Normalized Content Package, `ReadingDocument`
    view, source map и metadata.
12. Передать текстовые слои в поиск и будущие ИИ/learning pipelines.

### Выбор библиотек

Принятый основной parser:

- `comrak` - основной Markdown parser-кандидат. Причины: CommonMark + GFM
  compatibility, AST access, options/extensions, возможность не использовать
  HTML renderer и строить свой `ReadingDocument`.

Дополнительные библиотеки:

- `pulldown-cmark` - fallback/reference parser для performance experiments или
  streaming/event-based import. Не основной выбор, потому что Lumi важны AST,
  source map и extension control.
- `toml` - TOML front matter.
- `serde_saphyr` - YAML front matter candidate. `serde_yaml` не выбирать как
  новый dependency, потому что crate помечен deprecated/unmaintained.
- `ammonia` - defense-in-depth sanitizer для HTML fragments, если потребуется
  ограниченное отображение; не использовать как основной renderer.
- `url` - normalization для external links.

### Source map и anchors

Markdown importer должен сохранять source positions:

- byte ranges для blocks и inline nodes, где parser это позволяет;
- line ranges для diagnostics и future editing;
- generated heading ids;
- original link destination;
- normalized link target;
- resource path.

Если parser не дает точный range для некоторого inline node, importer использует
fallback: parent block range + quote/context anchor.

### Экспорт обратно в Markdown

Markdown importer не обязан сразу реализовывать editing, но модель должна не
блокировать будущий round-trip export:

- сохранять front matter raw/normalized form;
- сохранять source-specific link syntax, включая wikilinks;
- не терять code fence info string;
- не переписывать raw HTML без явного user action;
- сохранять unresolved links и missing resource references.

## Интеграции и зависимости

- **Reader.** Markdown importer выдает Normalized Content Package;
  `ReadingDocument` является reader-facing view поверх него. Reader отвечает за
  paginated rendering, anchors, заметки, timeline events и панели.
- **Reader architecture.** Markdown не зависит от Dioxus/WebView/native shell и
  не содержит platform-specific render state.
- **`lum`.** `lum` использует Markdown как основу, но добавляет manifest,
  multi-file book structure, interactive blocks и более строгий project model.
- **Obsidian.** Wikilinks, front matter, tags и callouts должны быть совместимы
  с будущей Obsidian-интеграцией, но Obsidian vault sync проектируется отдельно.
- **Поиск.** Markdown importer передает normalized text по headings/nodes в
  индекс текущего документа и глобальный индекс.
- **База знаний.** Markdown notes и imported pages могут становиться элементами
  knowledge base с сохранением source path and anchors.
- **ИИ.** Markdown importer не вызывает ИИ сам. Reader/background task может
  позже создать summary/questions/entities.
- **Плагины.** Math/LaTeX, Mermaid, SVG, code highlighting и interactive blocks
  идут через first-party plugin contracts.
- **Синхронизация.** Синхронизируются `Material`, `DocumentRevision`, resources
  metadata, anchors, progress и annotations. Исходный `.md` может
  синхронизироваться отдельно в зависимости от storage policy.

## Альтернативы

- `rejected`: рендерить Markdown напрямую в HTML и вставлять sanitized HTML в
  reader. Это ломает source map, anchors, unified rendering и plugin contracts.
- `rejected`: поддерживать все Markdown-диалекты сразу. Это создаст
  непредсказуемый import; нужен явный dialect contract.
- `rejected`: считать папку Markdown-файлов одной книгой по умолчанию. Для
  этого нужен `lum` manifest или Obsidian vault model.
- `rejected`: MDX как базовый Markdown input. MDX требует JS/JSX runtime и
  нарушает security boundaries.
- `rejected`: arbitrary HTML/JS widgets inside Markdown. Это превращает reader
  в browser runtime для недоверенного контента и ломает anchors, pagination,
  offline behavior and sync.
- `revisit`: разрешить JS-capable content только через будущий `lum-dynamic`
  или equivalent plugin capability profile with explicit user consent and
  sandboxing. Обычный Markdown не должен становиться таким runtime.
- `revisit`: `pulldown-cmark` как основной parser, если performance/source-map
  прототип покажет, что event-based import лучше AST.
- `revisit`: Pandoc-style extensions: definition lists, citations, attributes,
  footnotes. Часть из них полезна, но их нужно вводить явно.

## Открытые вопросы

- Делать ли footnotes обязательной extension для Markdown или оставить до
  `lumi-markdown`/plugin layer?
- Нужно ли поддерживать Markdown attributes syntax вроде `{#id .class}`?
- Какой набор raw HTML tags считать safe subset для прямого mapping в
  `ReadingNode`?
- Должны ли inline `#tags` становиться material tags автоматически или только
  после user confirmation?
- Как импортировать папку `.md` файлов вне `lum`: как набор материалов, как
  collection или через Obsidian/vault flow?

## Источники

- [CommonMark Spec](https://spec.commonmark.org/)
- [GitHub Flavored Markdown Spec](https://github.github.com/gfm/)
- [`comrak` crate](https://docs.rs/comrak/latest/comrak/)
- [`pulldown-cmark` crate](https://docs.rs/pulldown-cmark/latest/pulldown_cmark/)
- [`toml` crate](https://docs.rs/toml/latest/toml/)
- [`serde_saphyr` crate](https://docs.rs/serde-saphyr/latest/serde_saphyr/)
- [`ammonia` crate](https://docs.rs/ammonia/latest/ammonia/)
