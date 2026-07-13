# ADR 0005: EPUB parser stack и защитные лимиты S1

Status: accepted

## Контекст

EPUB является недоверенным ZIP-контейнером с XML, HTML, CSS, SVG и бинарными
resources. Выбранный importer обязан сохранить source-backed структуру, но не
должен исполнять или напрямую рендерить содержимое книги. До real import slice
нужно проверить конкретный Rust stack и установить численные ограничения,
которые применяются до больших allocations.

## Решение

Подтвердить стек из `docs/systems/formats/epub.md`:

- `zip` с отключёнными default features и только stored/Deflate support;
- `quick-xml` 0.41+ streaming reader для `container.xml`, OPF и NCX; более
  ранние версии не допускаются из-за RUSTSEC-2026-0194 и RUSTSEC-2026-0195;
- `scraper`/`html5ever` для tolerant parsing XHTML/HTML content documents;
- `url` для package-relative references;
- whitelist mapping DOM → `ReadingNode` как основную sanitizer boundary;
- `ammonia` только как defense-in-depth для разрешённых HTML fragments,
  которые ещё не стали typed nodes.

S1 применяет следующие defaults:

| Ограничение | Значение |
| --- | ---: |
| Upload/source EPUB | 100 MiB |
| ZIP entries | 10 000 |
| Суммарный uncompressed size | 512 MiB |
| Один resource | 64 MiB |
| `container.xml`, OPF или NCX | 2 MiB |
| Один XHTML/HTML content document | 8 MiB |
| Нормализованный archive path | 1024 bytes |
| Per-entry и aggregate compression ratio | 100:1 |

Лимиты являются конфигурацией deployment, но server не может поднимать их выше
без отдельной security/performance проверки. Клиентские значения не доверенные.

До чтения entry importer проверяет declared compressed/uncompressed sizes,
метод compression, encryption flag, safe enclosed path, отсутствие symlink и
duplicate normalized paths и совокупные counters. Фактическое чтение дополнительно ограничивается
`Read::take(limit + 1)`, поэтому ложный ZIP header не обходит лимит. Разрешены
только methods 0 (stored) и 8 (Deflate); split archive, ZIP encryption и nested
archive expansion отклоняются.

Package XML разбирается без DTD/external entities: любой `DOCTYPE` отклоняется,
а parser не выполняет network/filesystem resolution. Версии parser/normalizer и
правила canonical ordering входят в `importer_version`, чтобы одинаковый source
при той же версии давал детерминированные stable ids, package hash и source map.

`mimetype` проверяется как первый stored entry со значением
`application/epub+zip`. `META-INF/container.xml` обязателен. Remote schemes,
scripts, inline event handlers, iframes, foreign CSS и unsafe SVG не попадают в
reader. Unsupported content создаёт diagnostic/placeholder, если безопасно
продолжить; нарушение container/security limits завершает job как failed.
Fixed-layout metadata и DRM/encrypted resources детектируются до нормализации:
S1 возвращает явный unsupported/locked diagnostic и не направляет их в
reflowable `ReadingDocument` path.

Worker проверяет cancellation между entries и при streaming чтении. В logs
попадают code, media type, sizes и source path после безопасного redaction, но
не текст книги.

## Последствия

- 512 MiB expanded content нельзя держать целиком в памяти: resources идут в
  blob backend, XML обрабатывается streaming, XHTML — по одному spine item.
- Некоторые легитимные image-heavy EPUB потребуют понятной ошибки limit exceeded
  или административного изменения deployment policy.
- `ammonia` не превращает произвольный HTML в product model и не является
  основанием для `dangerous_inner_html`.
- Parser и sanitizer dependencies входят в security audit/upgrade cadence.

## Альтернативы

- Rust crate `epub` как production core: отклонено из-за GPL-3.0 и слишком
  высокоуровневой модели для полного source map.
- `epub.js`/iframe rendering: отклонено, потому что обходит общий
  `ReadingDocument`, anchor и pagination contracts.
- Распаковать во временную директорию обычной ZIP-командой: отклонено из-за
  path traversal, слабого контроля counters и лишней filesystem boundary.
- Sanitizer после прямого HTML render: отклонено; whitelist normalization в
  typed nodes является основной границей.

## Compatibility

- Исполняемый spike `spikes/stage0/src/epub.rs` создаёт минимальный EPUB 3,
  проходит container/OPF/XHTML stack и проверяет path traversal, compression
  ratio, entry count и active-content sanitization.
- Golden corpus Stage 2 добавляет EPUB 2/3 navigation, images, footnotes,
  tables, malformed XML, remote resources, SVG и ZIP bomb cases.
- Значения limit profile versioned как `epub-limits.s1`; diagnostics обязаны
  включать стабильный code и фактически превышенный limit.

Источники: [EPUB 3.3 OCF](https://www.w3.org/TR/epub-33/#sec-ocf),
[`zip`](https://docs.rs/zip/), [`quick-xml`](https://docs.rs/quick-xml/),
[`scraper`](https://docs.rs/scraper/) и
[`ammonia`](https://docs.rs/ammonia/).
