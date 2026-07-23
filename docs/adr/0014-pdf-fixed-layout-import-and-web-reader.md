# ADR 0014: PDF fixed-layout import и Web reader

Status: accepted

## Контекст

PDF нельзя публиковать как обычный `ReadingDocument`: координаты, физическая
страница и исходная верстка являются частью смысла документа. При этом
материал должен использовать те же durable job, revision, progress и
annotation boundaries, что EPUB и остальные источники.

Серверу нужен безопасный и воспроизводимый способ извлечь metadata, геометрию,
нативный text layer и thumbnail. Browser должен сохранять fidelity, не
передавать bitmap в reactive Rust state и не хранить anchors в CSS pixels.

## Решение

- `.pdf` принимается существующим `POST /api/v1/imports` с теми же session,
  CSRF и idempotency boundaries. Лимит исходника — 200 MiB, документов — 5000
  страниц, видимой стороны страницы — 20 000 PDF points.
- Исходник становится content-addressed blob. Worker вызывает Poppler через
  adapter `PdfEngine`, извлекает safe metadata, per-page geometry, native
  bbox text layer и thumbnail первой страницы. Типы Poppler не выходят из
  adapter boundary.
- Успешный импорт атомарно публикует immutable `DocumentRevision` и
  `FixedLayoutContentPackage`. Reader получает отдельный
  `PageFidelityDocument`; endpoint `ReadingDocument` для PDF не используется.
- Web renderer использует pinned `pdfjs-dist` в module worker. Исходник
  загружается owner-scoped endpoint с поддержкой single HTTP byte ranges.
  Страницы рисуются лениво в `canvas`; прозрачный text layer, link hit areas и
  annotation overlay являются отдельными слоями.
- PDF anchor хранит checksum исходника, физическую страницу, page hash,
  canonical page-point rectangles и normalized fallback. Device pixels, DOM
  path и текущий zoom не являются источником истины.
- Progress сохраняется как page anchor. Highlights и notes используют общий
  annotation API и не изменяют исходный PDF.
- Password UI, OCR, page-area notes без text selection, server-side outline и
  search index остаются следующими совместимыми расширениями. PDF без
  доступного native text layer уже открывается визуально и получает diagnostic
  `pdf_ocr_candidate`.

## Последствия

- Reflowable reader core не зависит от PDF.js, canvas, Poppler или platform
  handles.
- Web сохраняет визуальную точность PDF и получает selectable text и durable
  overlays без собственного PDF renderer.
- Production server image обязан содержать `poppler-utils`; Web image обязан
  собирать pinned PDF.js assets через `npm ci`.
- Отказ text extraction или thumbnail не обязан ломать визуальное чтение.
  Malformed, locked, oversized и неподдерживаемые документы завершают job
  структурированной diagnostic.
- Browser PDF.js повторно разбирает исходник для рендера. Серверный text layer
  остаётся authoritative производным артефактом для будущих search/export/AI
  задач, а не источником визуального слоя.

## Альтернативы

- Конвертация PDF в reflowable blocks отклонена: она теряет page fidelity и
  создаёт нестабильные anchors.
- Встроенный browser PDF viewer отклонён: он не предоставляет Lumi надёжный
  annotation overlay и source-backed selection contract.
- Серверный bitmap на каждую страницу отклонён как основной Web path: он
  увеличивает storage/traffic и ухудшает zoom/text selection.
- Запись highlights внутрь исходника отклонена: это нарушает immutable source
  и усложняет sync/conflicts.

## Совместимость

- Migration `20260723120000_pdf_fixed_layout.sql` добавляет `pdf` в durable
  source boundary без изменения существующих EPUB rows.
- Domain marker: `s1.2026-07-23.pdf-fixed-layout-v1`, migration catalog entry
  `s1-0008-pdf-fixed-layout`; package marker: `normalized.fixed-layout.v1`.
- Pinned browser dependency: `pdfjs-dist@6.1.200`.
- Детерминированная фикстура `tests/fixtures/pdf/text-layer.pdf` проверяет
  portrait/landscape geometry, кириллицу, native text и links.
- Unit suite проверяет package limits и HTTP Range; Poppler integration
  проверяет metadata, text layer и thumbnail; PostgreSQL test проверяет
  publication, progress, annotation и source download; Playwright проходит
  upload → render → selection → highlight.
