# ADR 0006: Browser-measured pagination и PageMap

Status: accepted

## Контекст

Reflowable reader должен давать page-like navigation, сохраняя anchors при
смене темы, шрифта, ширины и viewport. Собственный text layout engine
дублировал бы browser layout, а CSS columns требуют держать длинный документ в
DOM и плохо дают source-backed page boundaries. Нужен алгоритм, который
использует browser measurement, но выдаёт platform-independent `PageMap`.

## Решение

Web adapter строит страницы инкрементально в скрытом measurement container:

1. Входом служат `ReadingNode` и render plan, а не EPUB XHTML.
2. Layout key включает `DocumentRevision`, viewport bucket, font family/size,
   line height, content width, theme metrics и версии загруженных fonts/resources.
3. Adapter наполняет page-sized container блоками в source order и измеряет
   `scrollHeight`/`getBoundingClientRect` после готовности fonts и images.
4. Если текстовый block не помещается, граница ищется binary search по
   character offset через DOM `Range`. Канонический offset — индекс Unicode
   scalar value, как в shared Rust `TextRange`; web adapter явно конвертирует
   его в/из DOM UTF-16 offsets. Break нормализуется до grapheme/word boundary,
   если это не создаёт пустую страницу.
5. Figure, короткая table, plugin placeholder и другие atomic blocks
   переносятся целиком на следующую страницу через `break-inside: avoid`.
   Oversized table получает
   внутренний horizontal scroll, oversized image масштабируется с сохранением
   пропорций; это отражается diagnostic/measurement hint.
6. Footnote marker остаётся в тексте, а footnote body является reader-native
   target. Короткое примечание может быть atomic block; длинное допускает
   продолжение с тем же node id и offset range.
7. Каждая страница записывает start/end `PageBoundary` как node path, text
   offsets и optional source locator. DOM node, pixel coordinate и page number
   не являются durable position.
8. При смене layout key `PageMap` пересчитывается, а текущая позиция
   восстанавливается по source-backed boundary/anchor. Derived PageMap можно
   кешировать, но не синхронизировать как source of truth.
9. В DOM находятся measurement window, текущая страница и небольшой соседний
   window. Pagination controller может продолжать вычисление вперёд idle
   batches и не требует активного DOM всей книги.

Первые версии используют Unicode scalar offsets из normalized node и сохраняют
half-open ranges `[start, end)`. UTF-8 byte и DOM UTF-16 offsets не попадают в
shared contract. Platform adapter обязан доказать полное неперекрывающееся
покрытие каждого текстового node в PageMap.

## Последствия

- Pagination соответствует фактическим browser fonts, tables и images без
  собственного layout engine.
- Page count является derived UI value и может измениться между devices и
  settings; progress хранится через locator/anchor, не `page 37`.
- Measurement требует browser test matrix и invalidation после late font/image
  load.
- Binary search уменьшает число layout passes, но pagination всё равно должна
  выполняться batches и иметь performance telemetry.

## Альтернативы

- CSS multi-column как финальный paginator: отклонено для основного path из-за
  all-DOM layout, сложного контроля atomic blocks и непрозрачных source ranges.
- Фиксированное число символов/слов на страницу: отклонено, потому что игнорирует
  typography, viewport, tables и media.
- Server-side pagination: отклонено, поскольку server не знает platform fonts и
  viewport.
- Только scroll mode: отклонено как единственный S1 UX, но остаётся настройкой,
  использующей те же anchors без PageMap navigation.

## Compatibility

- Browser spike находится в `docs/visuals/pagination-spike/`, а Playwright test
  — в `tests/e2e/pagination-spike.spec.ts`.
- Fixture обязательно содержит длинный текст, Unicode grapheme sequences,
  image, table, footnote и plugin placeholder и проверяет непрерывность ranges
  после изменения font size/viewport.
- Shared Rust model Stage 4 должен иметь platform-neutral schema
  `PageBoundary`/`PageMap`; конкретный `PageMap` остаётся derived layout cache,
  а DOM measurement — detail web adapter. Durable/sync position хранится как
  anchor/locator, не как вычисленная карта страниц.
- Native adapters могут использовать CoreText/TextKit/WebView или Android
  layout, сохраняя ту же boundary semantics.
- Stage 4 реализует shared `RenderPlan`, `PageBoundary` и `PageMap`, browser
  measurement через DOM `Range`/`scrollHeight`, проверку непрерывного покрытия,
  memory cache и virtualized current-page DOM. Durable scope settings/progress
  закреплён в ADR 0008.
