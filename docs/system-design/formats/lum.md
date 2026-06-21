# Lum

Status: draft

## Контекст

`lum` - собственный формат Lumi для материалов, которые должны вести себя как
книга, но оставаться author-friendly plain-text проектом. Концептуально это
похоже на Obsidian-подход: папка с Markdown-файлами, локальными assets,
wikilinks, embeds, tags и внутренней graph-структурой. Главное отличие: `lum`
не является заметкой или произвольным vault. Это book-first формат с manifest,
spine, управляемой структурой, стабильными anchors и контролируемыми
interactive blocks.

Иными словами:

- Markdown - один файл или один материал.
- Obsidian vault - редактируемая база заметок.
- `lum` - переносимая книга/курс/long-form material, собранная из Markdown и
  ресурсов, но читаемая через единый reader.

Reader не редактирует `lum` как исходник. В reader пользователь читает,
аннотирует, делает заметки, проходит упражнения и создает knowledge artifacts.
Правка исходного `lum` может появиться позже в authoring tools, но в модели
чтения imported `lum` становится `DocumentRevision`, а пользовательские данные
живут отдельно от source package.

Продуктовая формулировка для `v01`: `lum` - это vault-like папка для авторинга
и book-like документ для чтения. Пользователь может думать о source project как
о папке с Markdown, но приложение не открывает ее как дерево заметок по
умолчанию. Lumi компилирует проект в `DocumentRevision` and Normalized Content
Package, reader строит единый `ReadingDocument`, `PageMap` и показывает
материал как сшитую книгу с сохраненными границами глав, source map и anchors.

`lum` должен учиться у книжных форматов, но не становиться их копией:

- EPUB дает лучшую модель package: single-file ZIP distribution, manifest,
  spine, navigation document, resource list, media types, fallback и validation.
- FB2 дает хорошую семантическую дисциплину: книга описывается через структуру
  и смысловые элементы, а не через финальную верстку.
- FB3 полезен как подтверждение направления "FB2 semantics + package with
  separate resources", но сам по себе не должен быть dependency или target для
  `lum`.

## Пользовательские сценарии

- Пользователь импортирует `.lum` файл как готовую книгу или курс.
- Пользователь импортирует папку с `lum.toml`, Markdown-файлами и assets как
  source project.
- Автор или продвинутый пользователь собирает книгу из нескольких `.md` глав,
  изображений, диаграмм, упражнений и ссылок.
- При открытии source project читатель видит не файловое дерево, а единый
  постраничный документ, сшитый по `spine`.
- Читатель видит единый book-like материал: обложка, metadata, оглавление,
  части, главы, приложения, прогресс и постраничное чтение.
- Читатель переходит по internal links, wikilinks, backlinks, сноскам,
  glossary/concept links и embedded references.
- Читатель проходит встроенные упражнения, карточки, вопросы, code examples,
  diagrams и другие first-party plugin blocks.
- Читатель делает хайлайты, заметки, voice notes и записи на полях с устойчивой
  привязкой к главе, блоку или фрагменту.
- Lumi использует `lum` metadata для поиска, базы знаний, обучения и ИИ-задач,
  не смешивая исходный контент с личными заметками пользователя.

## Функциональные требования

### Формы хранения

`lum` должен поддерживать две формы:

- **Source project** - папка с обязательным `lum.toml`. Удобна для authoring,
  git, Obsidian-like workflows и ручного редактирования.
- **Package** - переносимый `.lum` файл. Технически это ZIP-архив с тем же
  `lum.toml` в корне, Markdown-файлами, assets и generated metadata. ZIP выбран
  для `v01`, потому что EPUB и FB3 уже идут в сторону ZIP-like package, а reader
  получает случайный доступ к отдельным главам и ресурсам без распаковки всего
  архива.

Source project и package должны давать одинаковый `ReadingDocument`, если
содержимое и настройки импорта совпадают.

Базовая структура:

```text
book-source/
  lum.toml
  content/
    00-preface.md
    01-introduction.md
    02-chapter.md
  assets/
    cover.jpg
    diagrams/
    images/
  data/
    examples.csv
  blocks/
    optional-large-block-data.json
```

Package `.lum` использует такую же структуру внутри архива.

Package rules:

- `.lum` - ZIP archive with constrained features, not arbitrary ZIP.
- Source project может не содержать package-only files like `mimetype` and
  `META-INF/`; `lum pack` добавляет их при сборке архива.
- Root `mimetype` file should be the first entry, stored without compression,
  with value `application/vnd.lumi.lum+zip`, если это не усложнит раннюю
  реализацию. Это повторяет полезный EPUB-паттерн fast identification.
- Root `lum.toml` is mandatory and remains the package document.
- `META-INF/` reserved for future signatures, hashes, compatibility metadata
  and build diagnostics.
- Allowed compression for `v01`: stored and deflate. Другие algorithms не
  нужны до появления убедительного production case.
- Symlinks, hardlinks, absolute paths, platform-specific permissions and file
  metadata are ignored or rejected.
- Entry names are normalized as UTF-8 package-relative paths.

TAR/TAR.GZ не выбран как основной package для `v01`: он лучше для streaming
архивов и Unix tooling, но хуже подходит для random access к ресурсам книги,
web/server processing, partial extraction, per-entry metadata и привычной
экосистемы ebook containers.

### Manifest

`lum.toml` - обязательный manifest. Он задает идентичность книги, порядок
чтения, metadata, capabilities, ресурсы и правила импорта.

Минимальный пример:

```toml
format_version = "0.1"

[book]
id = "example-book"
title = "Example Book"
subtitle = "Optional subtitle"
language = "ru"
authors = ["Author Name"]

[[spine]]
id = "preface"
path = "content/00-preface.md"
title = "Preface"

[[spine]]
id = "introduction"
path = "content/01-introduction.md"
title = "Introduction"

[features]
markdown = "lumi-markdown"
required_plugins = ["lumi.code", "lumi.svg"]
optional_plugins = ["lumi.math", "lumi.mermaid", "lumi.quiz"]
```

Manifest responsibilities:

- format version;
- stable book id;
- title, subtitle, authors, language, publisher/source metadata;
- cover and other primary resources;
- spine: ordered reading sequence;
- navigation groups: TOC, landmarks, page-list/reference pages, list of figures
  and tables, glossary;
- optional parts/sections grouping and reading progression direction;
- included files, media types, roles, hashes and assets policy;
- required/optional first-party plugins;
- fallback policy for unsupported resources and plugin blocks;
- external resource policy;
- accessibility metadata: alt text requirements, language, direction and
  semantic landmarks;
- search/learning/AI hints, если они включены автором;
- compatibility constraints.

Manifest должен быть строгим: unknown top-level sections запрещены или
сохраняются как extension metadata только при явном namespace, например
`[x.vendor]`. Это снижает риск тихой несовместимости.

### Spine и структура книги

`spine` - главное отличие `lum` от Markdown-папки и Obsidian vault.

Правила:

- Все основные читаемые главы должны быть перечислены в `spine`.
- Порядок `spine` определяет порядок чтения, прогресс, оглавление и
  pagination chunks.
- Markdown-файлы вне `spine` могут быть references, appendices, glossary,
  hidden notes или source-only files, но не входят в линейное чтение
  автоматически.
- Каждый spine item получает stable `id`.
- Один Markdown-файл может быть одной главой; деление на внутренние sections
  берется из headings.
- Parts/sections могут задаваться в manifest или выводиться из heading
  hierarchy, но manifest имеет приоритет.
- Spine item может иметь `linear = false`, если файл входит в package как
  appendix/reference и доступен через ссылки, но не является частью основного
  маршрута чтения.
- Для языков с другим направлением чтения manifest может задавать
  `reading_direction`, но reader все равно строит страницы через общую
  layout/pagination model.

Book graph существует рядом со spine, но не заменяет его. Wikilinks и backlinks
дают сеть смысловых связей, а spine дает читателю маршрут.

Сшивка Markdown-файлов не должна быть простой конкатенацией исходников. Importer
сохраняет границы глав, headings, source ranges и resource refs, а reader
показывает их через общий `ReadingDocument` и постраничный `PageMap`.

### Markdown-основа

Контент глав пишется в `lumi-markdown`, описанном в
[`markdown.md`](markdown.md):

- CommonMark + GFM baseline;
- YAML/TOML front matter;
- wikilinks;
- embeds;
- callouts;
- tags;
- local resources;
- safe raw HTML policy.

`lum` не переопределяет Markdown parser. Он использует Markdown importer как
внутренний слой, но добавляет book-level manifest, resolution, validation,
resource graph, plugin block compilation и cross-file anchors.

Front matter в главе может дополнять manifest:

- `title` - chapter title, если manifest не задал title;
- `description` - chapter description;
- `role` - semantic role: preface, chapter, appendix, notes, glossary,
  bibliography, index;
- `tags` - chapter tags;
- `draft` - файл нельзя включать в package без explicit allow;
- `weight` - authoring hint, не заменяет `spine`;
- `concepts` - связанные concepts/glossary entries.

Manifest сильнее front matter для book-level решений.

### Links, wikilinks и backlinks

`lum` поддерживает Obsidian-like linking, но с book-level rules:

- `[[chapter]]` - ссылка на главу или note/resource id.
- `[[chapter#heading]]` - ссылка на heading внутри главы.
- `[[chapter#heading|alias]]` - ссылка с alias.
- Markdown links к локальным `.md` файлам резолвятся в internal material links.
- Links к assets резолвятся в package resources.
- Backlinks строятся как generated index, а не как пользовательский контент.
- Broken links не ломают импорт; они становятся `LumImportIssue` и видимыми
  unresolved links.

Resolver должен уметь различать:

- spine chapter;
- non-spine reference note;
- glossary/concept entry;
- resource;
- external URL;
- unresolved target.

### Resources

Все локальные resources должны быть частью source project/package или явно
объявлены external.

Поддерживаемые группы:

- images: JPEG, PNG, GIF, WebP, SVG через first-party SVG capability;
- media: audio/video как future reader/plugin resources;
- data: JSON, CSV, TOML for interactive blocks and examples;
- downloads/supplements: optional attachments;
- generated indexes: search hints, backlinks, concept graph, если они созданы
  build step.

Resource rules:

- Paths всегда package-relative.
- Абсолютные пути запрещены.
- `..` path traversal запрещен.
- В ZIP package запрещен zip-slip.
- Resources are declared in manifest or discovered by import with diagnostics;
  package build should be able to produce an exhaustive resource list.
- Resource records include path, media type, role, content hash, size and
  required/optional policy.
- Unsupported required resource blocks import; unsupported optional resource
  creates placeholder and warning.
- Один и тот же resource должен иметь content hash.
- Missing resource не должен падать весь import, если resource optional.
- Required resource создает import error и блокирует package, если без него
  нарушается содержание.

Главный урок FB2/FB3: не встраивать крупные binary payloads внутрь Markdown или
TOML. Images, media, datasets and generated indexes должны быть отдельными
package resources с hashes and media types.

### Book semantics

Помимо обычных Markdown-блоков, `lum` должен иметь нормализованные книжные
семантики, вдохновленные FB2 и EPUB:

- annotation/abstract;
- dedication;
- epigraph;
- preface/foreword/afterword;
- part/chapter/section;
- poem/stanza/verse line;
- blockquote/citation with attribution;
- footnotes/endnotes/comments body;
- bibliography/references;
- glossary/concepts;
- list of figures/tables;
- cover/title page.

Source syntax can stay Markdown/front matter/callout based, but compiled model
should preserve these roles as `ReadingNode` metadata. Reader decides final
visual style; source project does not impose book-specific layout.

### Interactive blocks

`lum` - первый формат, где interactive blocks являются штатной частью
содержания, а не только reader overlay.

Базовое правило: interactive block декларативен. Он не приносит произвольный JS,
HTML или platform-specific runtime. Он превращается в typed `ReadingNode` /
plugin block и исполняется через first-party plugin contract.

В `lum` есть два уровня расширенных Markdown-блоков:

- **Rich Markdown fences**: `mermaid`, `math`, `latex`, `svg`, code fences and
  similar recognized blocks. Они обрабатываются общим `lumi-markdown` importer
  и мапятся в first-party plugin blocks.
- **LUM-native interactive fences**: `lum:<block_type>`. Они требуют book-level
  manifest/capability validation, могут ссылаться на package resources/data и
  могут иметь user state outside source package.

Raw HTML/JS не является третьим уровнем расширений. Если автору нужен dynamic
UI, он должен описать typed block и required plugin capability, чтобы reader
мог безопасно отрисовать placeholder, измерить блок для pagination и сохранить
anchors.

Future path: `lum-dynamic` может появиться как отдельный format/capability
profile, а не как поведение обычного `lum`. Такой материал явно объявляет, что
ему нужен JS-capable dynamic runtime, и открывается только после явного
согласия пользователя.
Reader должен показать, что материал может быть менее переносимым, хуже
пагинироваться, требовать sandboxed web surface и иметь более слабые guarantees
для anchors/offline/cross-platform rendering. Даже в этом режиме dynamic blocks
должны идти через plugin contract, trust levels, capability prompts, resource
limits и visible fallback, а не через raw HTML из Markdown, который исполняется
незаметно для пользователя.

Первичный portable syntax - fenced code block с `lum:<block_type>`:

````markdown
```lum:quiz
id = "q-intro-1"
type = "single_choice"
question = "What is the main idea?"
options = ["A", "B", "C"]
answer = "B"
```
````

Причины выбрать fenced syntax как базовый вариант:

- совместим с обычным Markdown parser;
- безопасно отображается как code block вне Lumi;
- сохраняет source ranges;
- не требует исполнения HTML/JS;
- удобно валидируется как typed payload.

В будущем можно добавить author-friendly directive syntax, но compiled model
все равно должен быть typed plugin block.

First-party block families:

- `lum:quiz` - вопросы, тесты, self-check;
- `lum:flashcard` - карточки и повторение;
- `lum:exercise` - открытые задания;
- `lum:code` - executable/read-only code examples через безопасный plugin;
- `lum:diagram` - Mermaid/SVG/diagram blocks;
- `lum:chart` / `lum:viz` - declarative data visualizations over local package
  data/resources;
- `lum:math` - Math/LaTeX;
- `lum:media` - audio/video overlays;
- `lum:embed` - вложенный ресурс или reference material;
- `lum:callout` - typed callout, если обычного Markdown callout недостаточно.

Конкретная реализация этих блоков проектируется в документе по плагинам и
learning-документе. `lum` только задает serialization, validation и mapping в
reader.

### Knowledge и learning metadata

`lum` может не только содержать текст, но и задавать авторскую учебную
структуру:

- concepts;
- glossary;
- prerequisites;
- learning objectives;
- exercises;
- spaced-repetition hints;
- difficulty;
- estimated reading time;
- relationships between chapters and concepts.

Эта metadata не заменяет learning-подсистему. Она является входными данными.
История ответов, расписание повторений, персональная аналитика и адаптация
остаются в learning-документе.

### Read-only source и пользовательский слой

Imported `lum` source считается immutable для конкретного `DocumentRevision`.

Отдельно хранятся:

- reading progress;
- highlights;
- notes;
- voice notes;
- answers to exercises;
- generated summaries;
- knowledge base items;
- social comments.

Если source project/package обновился, Lumi создает новый `DocumentRevision` и
мигрирует anchors через общую anchor-модель: node path, quote, context,
content hash и source map.

### Редактирование и authoring

Для `v01` редактирование исходного `lum` внутри reader не входит в основной
контракт. Допустимые early paths:

- редактировать source project во внешнем редакторе или Obsidian-like workflow;
- явно переимпортировать/обновить source project в Lumi;
- получить новый `DocumentRevision` с попыткой миграции anchors;
- редактировать пользовательские заметки, хайлайты и KB entries отдельно от
  исходного `lum`.

Будущий authoring mode должен быть отдельным режимом или инструментом, а не
скрытой мутацией текущего reader document. Даже если Lumi позже получит
редактор `lum`, сохранение должно идти через source project/build step и
создавать новую revision, чтобы не ломать anchors, прогресс и историю
аннотаций.

## Нефункциональные требования

- **Book-first.** `lum` должен всегда иметь управляемый порядок чтения, а не
  только graph заметок.
- **Plain-text authoring.** Основное содержимое остается Markdown/TOML, чтобы
  формат был пригоден для git, diff, ручного редактирования и внешних tools.
- **Deterministic import.** Один и тот же source project/package должен давать
  одинаковый `ReadingDocument`, node ids, anchors и resource hashes.
- **Offline-first.** Package должен читаться без сети, если не объявлены
  external resources.
- **Security.** `lum` не исполняет произвольный JS/HTML. Interactive blocks
  проходят через plugin capability model.
- **Portability.** `.lum` должен быть самодостаточным package для передачи,
  синхронизации и архивирования.
- **Cross-platform.** `lum` не должен зависеть от web-only runtime; reader
  adapters получают уже нормализованный package, `ReadingDocument` view и typed
  plugin blocks.
- **Recoverability.** Broken links, missing optional resources and unsupported
  blocks должны давать diagnostics и placeholders, а не silent data loss.

## Модель данных

```text
LumSource
  -> LumManifest
  -> LumFileGraph
  -> Markdown chapters
  -> LumBookGraph
  -> LumCompiledDocument
  -> DocumentRevision
  -> Normalized Content Package
  -> ReadingDocument
```

Формат-специфичные сущности:

- `LumSource` - source folder или `.lum` package.
- `LumManifest` - parsed `lum.toml`.
- `LumPackage` - archive metadata, entries, hashes, size limits.
- `LumContainer` - validated `.lum` ZIP container with mimetype, root manifest
  and reserved metadata paths.
- `LumSpineItem` - ordered chapter/reference in reading route.
- `LumNavigation` - TOC, landmarks, page-list/reference pages, figures, tables
  and glossary navigation groups.
- `LumChapter` - Markdown source file with resolved metadata.
- `LumResource` - local/external resource with type, path, hash and policy.
- `LumLinkTarget` - resolved target for markdown links and wikilinks.
- `LumBookGraph` - chapters, links, backlinks, concepts, glossary and embeds.
- `LumPluginBlock` - typed block parsed from `lum:<block_type>`.
- `LumSourceMap` - mapping from source files/ranges to `ReadingNode`.
- `LumImportIssue` - warning/error with severity and source location.

Anchor source:

```text
LumAnchorSource {
  package_id
  package_version
  manifest_path
  chapter_id
  source_path
  byte_start
  byte_end
  line_start
  line_end
  heading_path
  block_id
  content_hash
}
```

Primary anchor все равно остается общей моделью Lumi, описанной в
[`../reading-screen.md`](../reading-screen.md) и
[`../reader-architecture.md`](../reader-architecture.md). `LumAnchorSource`
только добавляет форматное происхождение.

## Реализация

### Pipeline импорта

1. Определить source type: folder with `lum.toml` или `.lum` archive.
2. Для package: открыть constrained ZIP, проверить `mimetype`, paths, size
   limits, compression ratio, entry count and manifest presence.
3. Распарсить `lum.toml`.
4. Проверить `format_version`, required fields, `book.id`, `spine`.
5. Построить file/resource graph with media types, roles, hashes and required
   policies.
6. Распарсить Markdown chapters через `lumi-markdown` importer.
7. Сопоставить manifest spine с главами и generated headings.
8. Разрешить Markdown links, wikilinks, embeds, resources and concept refs.
9. Распарсить `lum:<block_type>` fenced blocks в typed plugin block payloads.
10. Провалидировать required plugins and capabilities.
11. Построить `LumBookGraph`, TOC, backlinks, glossary/concept indexes.
12. Скомпилировать Normalized Content Package and `ReadingDocument` view with
    stable node ids and source map.
13. Создать `DocumentRevision`, resource records and import diagnostics.
14. Передать text layers в поиск, learning/AI hints - в соответствующие
    фоновые pipelines.

### Выбор библиотек

Основные dependencies:

- `toml` - manifest and TOML front matter.
- `serde` - manifest/data schema deserialization.
- `schemars` - generated JSON Schema or validation documentation for manifest.
- `semver` - format/plugin compatibility constraints.
- `zip` - `.lum` package. Для `v01` используем constrained ZIP: stored/deflate,
  normalized UTF-8 paths, no symlinks/hardlinks and strict size limits.
- `camino` - UTF-8 package-relative paths.
- `blake3` или `sha2` - content hashes for files, resources and revisions.
- `comrak` - Markdown AST через уже выбранный Markdown importer.
- `serde_saphyr` - YAML front matter, если используется в главах.
- `url` - external URL normalization.
- `ammonia` - defense-in-depth only for limited safe HTML fragments.

Для package validation нужны собственные проверки поверх `zip`: path traversal,
entry size limits, total unpacked size, duplicated paths, suspicious compression
ratio, unsupported file types, unsupported compression methods and invalid
metadata.

### Stable IDs

`lum` должен генерировать стабильные ids:

- book id из manifest;
- chapter id из `spine.id`;
- heading id из Markdown heading algorithm;
- block id из explicit `id` или deterministic hash source range;
- resource id из package path + content hash;
- document revision id из manifest + normalized source hashes.

Если interactive block участвует в answers/learning/analytics, explicit `id`
желателен. Generated id допустим для отображения, но слабее для долговременной
миграции.

### Build and validation

Нужен отдельный validator/build step:

```text
lum validate <path>
lum pack <source-folder> --output book.lum
lum inspect book.lum
```

Это может быть CLI/dev-tool, а не пользовательская функция reader. Reader import
должен выполнять тот же validation core, чтобы не доверять package на входе.

Validation levels:

- `error` - import невозможен;
- `warning` - import возможен с placeholders/degraded behavior;
- `info` - authoring hint.

Examples:

- missing manifest: error;
- missing/invalid package `mimetype`: warning in early builds, error after
  package format stabilizes;
- duplicate spine id: error;
- required plugin unavailable: error or blocked material state;
- missing optional image: warning;
- unresolved wikilink: warning;
- unused asset: info;
- undeclared generated file in package: info/warning depending on policy;
- draft chapter included in package: warning/error depending on policy.

## Интеграции и зависимости

- **Reader.** `lum` импортируется в Normalized Content Package; обычный
  `ReadingDocument` является reader-facing view поверх него. Reader отвечает за
  post-paginated rendering, overlays, annotations, timeline and panels.
- **Markdown.** `lum` использует `lumi-markdown` as chapter syntax, но не
  равен Markdown. Manifest/spine/resource graph обязательны для book behavior.
- **Плагины.** Interactive blocks мапятся в first-party plugin blocks. Plugin
  document определит capability model, sandboxing, rendering and storage.
- **Learning.** Exercises, flashcards and objectives из `lum` становятся
  входными данными learning-подсистемы; персональные результаты хранятся вне
  source.
- **Knowledge base.** Concepts, glossary, links and user notes могут создавать
  KB entries с обратной ссылкой на `LumAnchorSource`.
- **Obsidian.** `lum` должен быть совместим с Obsidian-like authoring:
  Markdown, wikilinks, assets, tags and callouts. Vault sync/export будет
  описан отдельно в Obsidian-документе.
- **Search.** Index строится по compiled `ReadingDocument`, plus manifest
  metadata, concepts, glossary and headings.
- **ИИ.** Manifest/chapter metadata can provide AI context and permissions, но
  ИИ-задачи запускаются reader/AI layer, not importer.
- **Sync.** Синхронизируются package/source identity, `DocumentRevision`,
  resources metadata, progress, annotations and exercise answers. Сам source
  package может sync-иться как blob/content-addressed asset.

## Альтернативы

- `rejected`: считать `lum` просто папкой Markdown без manifest. Это повторяет
  Obsidian vault, но не дает книге spine, stable reading order, validation and
  package semantics.
- `rejected`: делать `lum` редактируемой заметкой внутри reader. Это смешивает
  source editing с reading/annotation layer и ломает revision/anchor model.
- `rejected`: использовать EPUB как внутренний `lum`. EPUB полезен для
  публикации, но хуже подходит для Obsidian-like authoring, knowledge graph,
  typed learning blocks and first-party plugin blocks.
- `rejected`: использовать MDX/JSX как основу interactive content. Это тянет
  JS runtime, security surface and platform-specific rendering.
- `rejected`: один огромный Markdown-файл как основной формат книги. Это хуже
  для authoring, links, assets, chapter-level metadata and partial reimport.
- `rejected`: TAR/TAR.GZ as primary `.lum` package for `v01`. TAR is good for
  streaming and simple archive tooling, but ZIP fits ebook packages better:
  random access, per-entry compression, browser/server/library support and EPUB
  precedent.
- `revisit`: `lum-dynamic` as separate opt-in format/capability profile for
  JS-capable interactive books. It must require explicit user consent,
  sandboxed plugin runtime, trust/capability prompts, resource limits and clear
  degraded guarantees for pagination, anchors, offline and cross-platform
  rendering.
- `revisit`: поддержать directory package with `.lum/` suffix. Сейчас достаточно
  папки с `lum.toml` and archive `.lum`.
- `revisit`: binary package format вместо ZIP, если ZIP окажется слабым для
  больших media/resources or streaming.
- `revisit`: signed packages and publisher identity. Нужны, если появится
  marketplace/public sharing.

## Открытые вопросы

- Нужен ли `lum.lock` для фиксирования generated indexes, plugin versions and
  exact resource hashes?
- Должен ли `.lum` package включать precompiled `ReadingDocument` cache или
  всегда компилироваться на устройстве?
- Какой exact namespace использовать для first-party plugins:
  `lumi.quiz`, `lum.quiz`, `reader.quiz`?
- Нужно ли разрешать non-spine Markdown notes внутри package как private author
  notes, или это должно жить только в source project?
- Как `lum` будет экспортироваться обратно в EPUB/PDF/Markdown bundle, если
  пользователь захочет вынести книгу из Lumi?

## Источники

- [W3C EPUB 3.3](https://www.w3.org/TR/epub-33/)
- [`epub.md`](epub.md)
- [`fb2.md`](fb2.md)
