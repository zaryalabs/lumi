# Нормализованный контент

Status: accepted

## Контекст

Lumi импортирует разные источники: EPUB, FB2, PDF, web pages, Telegram, X,
Markdown и `lum`. Reader, поиск, обучение, ИИ, социальные слои и экспорт не
должны зависеть от того, из какого источника пришел материал.

Для этого между format importer и downstream-подсистемами нужен внутренний
нормализованный слой: **Normalized Content Package**. Это не пользовательский
формат и не замена `lum`. `lum` остается authoring/package-форматом для людей,
а Normalized Content Package является результатом импорта конкретной версии
материала.

## Пользовательские сценарии

- Пользователь импортирует EPUB, web article или Telegram thread и получает
  одинаковые reader, annotations, search и AI actions.
- Пользователь повторно импортирует изменившийся источник. Lumi создает новую
  версию материала и пытается перенести anchors без потери старых заметок.
- Пользователь экспортирует материал и заметки. Экспорт содержит source
  provenance, quote, anchor и readable metadata.
- Участники shared room используют разные копии одной книги. Lumi сопоставляет
  версии через fingerprints и пытается перенести shared anchors.

## Функциональные требования

### Material и DocumentRevision

`Material` - стабильная библиотечная запись. Она хранит пользовательское
состояние: title override, tags, collections, active revision, archive/delete
state и social/share metadata.

`DocumentRevision` - immutable результат одного успешного импорта. Повторный
импорт не переписывает старую revision, а создает новую:

```text
Material
  id
  kind
  canonical_title
  active_revision_id
  library_state
  source_identity

DocumentRevision
  id
  material_id
  source_hash
  normalized_hash
  importer_id
  importer_version
  package_format_version
  created_at
  supersedes_revision_id?
```

Смена active revision является явной командой. Anchors, заметки и прогресс не
переносятся молча: resolver вычисляет confidence и сохраняет unresolved state,
если автоматический перенос небезопасен.

### Package forms

Normalized Content Package имеет две формы:

- **Reflowable package** для EPUB, FB2, web pages, Telegram, X, Markdown и
  `lum`.
- **Fixed-layout package** для PDF и fixed-layout EPUB.

Reflowable package содержит:

```text
normalized-package/
  manifest.json
  units.jsonl
  blocks.jsonl
  navigation.json
  resources/
  source-map.jsonl
  diagnostics.json
```

Fixed-layout package содержит:

```text
normalized-package/
  manifest.json
  pages.jsonl
  text-layer/
  outline.json
  resources/
  source-map.jsonl
  diagnostics.json
```

`manifest.json` хранит metadata, source provenance, capabilities, language,
reading order, fingerprints, importer version и links to source blobs.

### ReadingDocument и PageFidelityDocument

`ReadingDocument` - reader-facing view model для reflowable package. Он не
является исходным пользовательским файлом и не должен смешиваться с source
format.

`PageFidelityDocument` - reader-facing view model для fixed-layout package. Он
сохраняет страницы, координаты, text layer и overlay mapping.

Pipeline:

```text
Source artifact
  -> format importer
  -> DocumentRevision
  -> Normalized Content Package
  -> ReadingDocument | PageFidelityDocument
  -> reader/search/learning/AI/social layers
```

### Stable blocks и anchors

Каждый content unit/block получает stable id, source locator, text hash and
diagnostics. Anchor target хранит несколько селекторов:

- normalized block path and offsets;
- quote;
- prefix/suffix context;
- source locator: EPUB CFI, XML path, DOM path, Telegram/X id, Markdown source
  range, `lum` source range;
- PDF/fixed-layout page coordinates where relevant.

Resolver проходит ступени:

1. exact revision + block/offset;
2. exact quote in same block;
3. quote + context inside content unit;
4. source locator + checksum;
5. fuzzy local match with bounded confidence;
6. unresolved anchor requiring manual repair.

### Provenance и diagnostics

Каждая revision сохраняет:

- original source artifact ref;
- importer id/version;
- source URL/message id/file name/API response id where applicable;
- normalized package hash;
- content fingerprints for matching and shared rooms;
- structured import diagnostics.

Diagnostics являются частью revision audit trail, но не пользовательским
контентом. Их можно экспортировать для отладки и compatibility tests.

### Составной provenance Telegram/Web

Составной Telegram material использует один package и один revision, но не
стирает provenance отдельных секций. `unit-0` и его text/image blocks имеют
Telegram locator; каждая раскрытая страница образует следующий `ContentUnit` и
сохраняет Web locator исходного snapshot. Fallback unit нераскрытой ссылки
хранит исходный HTTP(S) URL и безопасную diagnostic без сетевых подробностей.
Ссылки, которые после capture имеют один `canonical_url`, раскрываются один раз;
повторные вхождения остаются в исходном Telegram text block и получают
диагностику дедупликации.

При переносе блоков Web normalizer заново формирует уникальные block ids,
`node_path`, navigation targets и internal targets относительно нового unit.
Для raw HTML он сначала выбирает содержательный semantic container, затем
ограниченно оценивает плотные `div`/`section`, а при отсутствии структурных
блоков использует bounded `text_content` snapshot как последний fallback.
Изображения ссылаются на content-addressed `resource_hash`; envelope, snapshots
и разрешённые изображения представлены в manifest. Поэтому anchors и progress
остаются общими для web, desktop и mobile и не зависят от DOM-путей Telegram
секции.

## Нефункциональные требования

- **Determinism.** Один source artifact при той же версии importer должен давать
  одинаковые stable ids, package hash and source map там, где это возможно.
- **Portability.** Source blobs, normalized metadata, anchors and notes can be
  exported without proprietary server-only meaning.
- **Isolation.** Внешние parser/renderer types не выходят за importer/adapter
  boundary.
- **Recoverability.** Старые revisions сохраняются достаточно долго, чтобы
  перенести anchors или явно подтвердить удаление.
- **Compatibility.** Изменение package schema требует ADR, migration notes and
  fixtures.

## Модель данных

```text
Material
  -> DocumentRevision[]
  -> NormalizedContentPackage
  -> ContentUnit[]
  -> ContentBlock[]
  -> ResourceManifest
  -> SourceMap
  -> ImportDiagnostics
```

Основные сущности:

- `Material` - stable library item.
- `DocumentRevision` - immutable imported version.
- `NormalizedContentPackage` - internal normalized package for a revision.
- `ContentUnit` - chapter, section, page, post or message group.
- `ContentBlock` - paragraph, heading, figure, table, code, plugin block etc.
- `ResourceManifest` - content-addressed local/cloud resources.
- `SourceMap` - mapping from normalized blocks to source locators.
- `ImportDiagnostic` - structured warning/error/info.
- `ContentFingerprint` - matching signal for versions and social rooms.

## Реализация

Importers write package artifacts and metadata through application services. Web
stores packages and resources in cloud storage. Native clients store packages in
local SQLite/blob storage after sync/import.

Schema versions:

- `normalized_package_version`;
- source-format importer version;
- source-map selector version;
- fingerprint version.

Any breaking change must include compatibility fixtures and migration strategy.

## Интеграции и зависимости

- **Reader.** Builds `ReadingDocument` or `PageFidelityDocument` from package.
- **Форматы.** Importers output `DocumentRevision` + package + diagnostics.
- **Синхронизация.** Native clients sync revision metadata and blobs/packages
  according to storage policy; web uses cloud package as primary source.
- **Поиск.** Indexes package text layers and source metadata.
- **Learning.** Learning items reference revision/unit/block anchors.
- **ИИ.** Context packs cite package chunks and source refs.
- **Social.** Shared material matching uses fingerprints and anchor recovery.
- **Плагины.** Format/importer plugins must output this package contract, not
  custom reader state.

## Альтернативы

- `rejected`: let every importer output its own reader model. This fragments
  anchors, search and AI context.
- `rejected`: use `lum` as the universal internal format. `lum` is an
  author-facing format; internal normalized packages need importer diagnostics,
  fixed-layout support and source-specific provenance.
- `rejected`: make Markdown the only internal representation. It is too lossy
  for stable anchors, tables, PDF text geometry, resources and interactive
  blocks.

## Открытые вопросы

- Exact serialization format for `units.jsonl`, `blocks.jsonl` and source maps.
- Retention policy for old `DocumentRevision` packages after anchor migration.
- Which fingerprints are safe enough for social matching without leaking too
  much source text.
