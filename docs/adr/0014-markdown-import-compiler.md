# ADR 0014: Markdown compiler и source locator

Status: accepted

## Контекст

Standalone Markdown должен открываться через общий reflowable reader, а будущий
`lum` обязан использовать тот же Markdown parser для глав. Прямой HTML render
обходит normalized package, anchors и security policy; отдельные standalone и
`lum` parser implementations неизбежно расходятся в heading ids, links и
diagnostics.

Добавление Markdown меняет source-format enum, selector/source-map contract и
domain schema marker, поэтому решение фиксируется отдельным ADR.

## Решение

- `lumi-core` предоставляет feature-gated `compile_markdown`. Он принимает
  decoded source, logical path, id внешнего content unit и default dialect, а
  возвращает metadata, normalized blocks, navigation, diagnostics и
  source-backed locators без создания account/storage state. Unit id не
  захардкожен: `lum` сможет безопасно объединять результаты нескольких глав без
  коллизий reader paths.
- Standalone `import_markdown` вызывает тот же compiler и упаковывает результат
  в один `ContentUnit`, `DocumentRevision` и `NormalizedContentPackage`.
  Будущий `lum` importer должен вызывать compiler по главе, добавляя
  manifest/spine/resource resolution снаружи.
- Основной parser — `comrak` 0.54 без HTML renderer. GFM включает tables,
  strikethrough, autolinks, task lists и tagfilter. `lumi-markdown` дополнительно
  включает alerts, wikilinks и footnotes.
- `MarkdownSourceLocator` хранит logical path, dialect, half-open byte range,
  one-based line range, heading path и optional generated heading id. Primary
  anchors продолжают использовать общий node path, quote/context и content
  hash.
- Heading ids строятся детерминированно из Unicode-текста; коллизии получают
  суффиксы `-2`, `-3` и далее. Same-document fragment links указывают на
  generated heading path.
- Raw HTML никогда не передаётся renderer как HTML. Block HTML становится inert
  plugin placeholder с recoverable source text и warning. Mermaid, math/LaTeX,
  SVG и `lumi:*` fences становятся typed capability placeholders; произвольный
  fence остаётся code block.
- Parser dependency включён feature `markdown-import`. Server включает feature,
  web/WASM собирает `lumi-core` без него и получает только сериализуемые domain
  contracts.
- Standalone upload принимает UTF-8 `.md`/`.markdown` до 10 MiB и не загружает
  external resources. Изображения пока становятся placeholders с diagnostic.

## Последствия

- Reader, pagination, annotations, progress, source download и sync projection
  не получают Markdown-specific ветку.
- `lum` сможет переиспользовать AST normalization и stable heading algorithm,
  но обязан отдельно реализовать container, manifest, spine, resource graph,
  cross-file links и capability validation.
- Начальный front matter reader поддерживает flat YAML/TOML-поля `title`,
  `author(s)`, `language` и `dialect`. Полная YAML/TOML data model, inline mark
  serialization, resolved standalone assets и round-trip editing остаются
  следующими additive slices.

## Совместимость и проверки

- Domain marker: `s1.2026-07-23.markdown-import-v1`.
- Normalized package marker остаётся `normalized.reflowable.s1`; новый
  `SourceLocator::Markdown` является additive enum variant.
- Migration `20260723180000_markdown_import.sql` расширяет durable import job
  source kinds и добавляет partial active-job index.
- Golden corpus `tests/fixtures/markdown` фиксирует GFM semantics и inert raw
  HTML. Unit tests проверяют determinism, headings, links, tables, task lists,
  front matter и malformed input; PostgreSQL test проверяет полный durable
  import и source download.

## Альтернативы

- Markdown → HTML → sanitizer: отклонено, потому что теряются AST semantics,
  source map и общий typed reader contract.
- Отдельный parser внутри `lum`: отклонено из-за несовместимых heading ids,
  diagnostics и extension policy.
- Сделать Markdown универсальным внутренним форматом: отклонено; он не покрывает
  fixed layout, resource fidelity и typed interactive payloads.
