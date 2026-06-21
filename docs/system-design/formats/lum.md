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

## Пользовательские сценарии

- Пользователь импортирует `.lum` файл как готовую книгу или курс.
- Пользователь импортирует папку с `lum.toml`, Markdown-файлами и assets как
  source project.
- Автор или продвинутый пользователь собирает книгу из нескольких `.md` глав,
  изображений, диаграмм, упражнений и ссылок.
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
  `lum.toml` в корне, Markdown-файлами, assets и generated metadata.

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
- optional parts/sections grouping;
- included files and assets policy;
- required/optional first-party plugins;
- external resource policy;
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

Book graph существует рядом со spine, но не заменяет его. Wikilinks и backlinks
дают сеть смысловых связей, а spine дает читателю маршрут.

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
- Один и тот же resource должен иметь content hash.
- Missing resource не должен падать весь import, если resource optional.
- Required resource создает import error и блокирует package, если без него
  нарушается содержание.

### Interactive blocks

`lum` - первый формат, где interactive blocks являются штатной частью
содержания, а не только reader overlay.

Базовое правило: interactive block декларативен. Он не приносит произвольный JS,
HTML или platform-specific runtime. Он превращается в typed `ReadingNode` /
plugin block и исполняется через first-party plugin contract.

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
  adapters получают уже нормализованный `ReadingDocument` и typed plugin blocks.
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
  -> ReadingDocument
```

Формат-специфичные сущности:

- `LumSource` - source folder или `.lum` package.
- `LumManifest` - parsed `lum.toml`.
- `LumPackage` - archive metadata, entries, hashes, size limits.
- `LumSpineItem` - ordered chapter/reference in reading route.
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
2. Для package: открыть ZIP, проверить paths, size limits, compression ratio и
   manifest presence.
3. Распарсить `lum.toml`.
4. Проверить `format_version`, required fields, `book.id`, `spine`.
5. Построить file/resource graph.
6. Распарсить Markdown chapters через `lumi-markdown` importer.
7. Сопоставить manifest spine с главами и generated headings.
8. Разрешить Markdown links, wikilinks, embeds, resources and concept refs.
9. Распарсить `lum:<block_type>` fenced blocks в typed plugin block payloads.
10. Провалидировать required plugins and capabilities.
11. Построить `LumBookGraph`, TOC, backlinks, glossary/concept indexes.
12. Скомпилировать `ReadingDocument` with stable node ids and source map.
13. Создать `DocumentRevision`, resource records and import diagnostics.
14. Передать text layers в поиск, learning/AI hints - в соответствующие
    фоновые pipelines.

### Выбор библиотек

Основные dependencies:

- `toml` - manifest and TOML front matter.
- `serde` - manifest/data schema deserialization.
- `schemars` - generated JSON Schema or validation documentation for manifest.
- `semver` - format/plugin compatibility constraints.
- `zip` - `.lum` package.
- `camino` - UTF-8 package-relative paths.
- `blake3` или `sha2` - content hashes for files, resources and revisions.
- `comrak` - Markdown AST через уже выбранный Markdown importer.
- `serde_saphyr` - YAML front matter, если используется в главах.
- `url` - external URL normalization.
- `ammonia` - defense-in-depth only for limited safe HTML fragments.

Для package validation нужны собственные проверки поверх `zip`: path traversal,
entry size limits, total unpacked size, duplicated paths, suspicious compression
ratio and unsupported file types.

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
- duplicate spine id: error;
- required plugin unavailable: error or blocked material state;
- missing optional image: warning;
- unresolved wikilink: warning;
- unused asset: info;
- draft chapter included in package: warning/error depending on policy.

## Интеграции и зависимости

- **Reader.** `lum` импортируется в обычный `ReadingDocument`; reader отвечает
  за post-paginated rendering, overlays, annotations, timeline and panels.
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
