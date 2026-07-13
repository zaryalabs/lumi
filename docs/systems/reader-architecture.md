# Архитектура экрана чтения

Status: accepted

## Контекст

Документ описывает техническую архитектуру reader: какие слои есть между
исходным форматом и экраном чтения, где проходит граница между нашим кодом и
готовыми rendering engines, как работает постраничность, anchors, overlays,
плагины и кроссплатформенность.

Это не замена [`reading-screen.md`](reading-screen.md). `reading-screen.md`
отвечает на вопрос "что должен уметь экран чтения"; этот документ отвечает на
вопрос "как он устроен технически".

Главный риск: попытаться написать собственный движок верстки текста. Lumi не
должен реализовывать свой HTML/CSS layout engine, line breaking, bidi,
hyphenation, selection engine и accessibility tree. Это задача низкоуровневого
layout/text engine платформы: browser engine в Web/WebView-сценарии или native
text/layout primitives в полностью нативном сценарии. Lumi должен владеть
моделью чтения, состоянием reader, anchors, overlays, постраничной навигацией,
заметками, поиском, обучением и plugin contracts, но использовать существующие
platform rendering engines для фактической раскладки текста.

## Архитектурное решение

Принимаем модель **custom reader over platform layout engines**:

- свой формат-независимый `ReadingDocument`;
- свой reader core: state, settings, navigation, anchors, annotations, timeline;
- свой слой pagination/page map;
- свои overlays для highlights, notes, comments и learning blocks;
- свои contracts для plugins;
- platform rendering engines для layout, text shaping, selection, accessibility
  и low-level painting. На web/desktop WebView/mobile WebView это browser
  engine, а при native mobile adapter - системный/native text stack.

Практически это означает:

- Web использует Dioxus Web, DOM, CSS и browser APIs.
- Desktop использует Dioxus Desktop/WebView path, пока он удовлетворяет
  требованиям reader.
- Mobile использует Dioxus Mobile/WebView path как основной кандидат, но core не
  должен зависеть от Dioxus Mobile напрямую.
- Если для mobile или desktop потребуется native shell, reader core и
  `ReadingDocument` остаются теми же, меняется только platform adapter.

## Слои

```text
Source formats
  -> Format importers
  -> DocumentRevision
  -> Normalized Content Package
  -> ReadingDocument | PageFidelityDocument
  -> Reader domain core
  -> Render plan
  -> Platform rendering adapter
  -> Layout measurement
  -> Page map
  -> Overlays and interactions
```

### Format Importers

Импортеры отвечают за превращение EPUB, FB2, web article, Telegram, X,
Markdown, `lum` и других источников в immutable `DocumentRevision` and
Normalized Content Package.

Импортеры не отвечают за UI, pagination, highlights и user interactions.

### Normalized Content Package

Normalized Content Package описан в
[`normalized-content.md`](normalized-content.md). Это persisted imported
content contract для конкретной `DocumentRevision`: metadata, reading order,
normalized units/blocks, resources, source map, diagnostics and fingerprints.

Это не пользовательский формат и не `lum`. Reader строит свои view models
поверх package, а не поверх исходного EPUB/HTML/PDF/Markdown.

### ReadingDocument

`ReadingDocument` - формат-независимая reader-facing модель чтения для
reflowable content. Она строится из Normalized Content Package и не содержит
DOM, WebView, Dioxus components, platform handles или CSSOM.

Модель должна быть:

- сериализуемой;
- пригодной для индексации;
- пригодной для синхронизации;
- стабильной для anchors;
- пригодной для рендера на web, desktop и mobile;
- расширяемой через first-party и future third-party plugin blocks.

### Reader Domain Core

Reader core содержит платформенно-независимую бизнес-логику:

- открытие документа;
- текущая позиция;
- reader settings;
- navigation state;
- page map state;
- anchors;
- annotations;
- bookmarks;
- timeline events для learning;
- reader tasks для ИИ;
- plugin registry и routing plugin blocks.

Reader core не должен импортировать `web_sys`, JavaScript types, DOM types,
Wry/WebView types, Android/iOS types или Dioxus renderer-specific APIs.

### Render Plan

Render plan - промежуточный слой между `ReadingDocument` и platform UI.

Он отвечает за:

- какие `ReadingNode` нужно отрисовать;
- какие атрибуты нужны для reverse mapping DOM/native selection -> anchor;
- какие plugin blocks нужно смонтировать;
- какие blocks участвуют в измерении страниц;
- какие resources нужно preload/defer;
- какие overlays нужно применить.

Render plan не занимается line breaking и не вычисляет пиксельную геометрию
текста сам.

### Platform Rendering Adapter

Platform adapter превращает render plan в конкретное отображение.

Web adapter:

- Dioxus Web components;
- HTML/CSS;
- browser DOM layout;
- browser Selection/Range APIs;
- `ResizeObserver`/`IntersectionObserver`/measurement APIs через adapter layer;
- sandboxed surfaces для опасного или foreign content.

Desktop adapter:

- Dioxus Desktop через system WebView/Wry как основной путь;
- тот же HTML/CSS-oriented reader surface, если WebView достаточно стабилен;
- native filesystem/window APIs через desktop services, а не через reader core;
- отдельный fallback path возможен через Tauri/WebView shell, если Dioxus
  Desktop окажется недостаточным.

Mobile adapter:

- Dioxus Mobile/WebView как основной кандидат;
- touch selection, gestures, bottom sheets и mobile-safe overlays как
  platform-specific слой;
- native bridges для файлов, voice notes, permissions и DRM;
- возможность заменить shell на native Android/iOS + WebView reader surface без
  переписывания importer и reader core.

## Rendering Strategy

### Что рендерим сами

Lumi рендерит:

- reader chrome: панели, toolbar, bottom sheets, context menus;
- структуру `ReadingNode` в HTML/native surface;
- highlights, notes, comments, bookmarks и learning overlays;
- plugin block containers;
- page navigation UI;
- fixed-layout/PDF annotation layer;
- placeholders/errors для неподдержанных blocks.

### Что не рендерим сами

Lumi не реализует:

- собственный text shaping engine;
- Unicode line breaking;
- bidi algorithm;
- hyphenation;
- glyph rasterization;
- font fallback;
- low-level selection engine;
- accessibility tree.

Эти задачи выполняет browser/WebView/native rendering engine. Поэтому слово
"браузер" в этом документе означает не обязательно пользовательский web browser,
а класс движков, которые уже умеют раскладывать и выделять сложный текст.

## Постраничность

Основной режим чтения - постраничный. Но постраничность Lumi - это не
самостоятельный typesetting engine.

Reader должен строить `PageMap`:

```text
PageMap {
  document_revision_id
  viewport
  reader_settings_hash
  page_count
  pages: Page[]
}

Page {
  page_index
  start_anchor
  end_anchor
  visible_node_ranges
  measured_rects
}
```

Принцип:

- platform adapter отрисовывает измеряемый фрагмент документа;
- layout engine платформы раскладывает текст;
- measurement layer считывает фактические размеры и rects;
- pagination controller строит `PageMap`;
- reader показывает окно страниц, а не весь документ целиком;
- при изменении viewport/settings/font/resources `PageMap` пересчитывается.

Возможные web/desktop/mobile техники:

- CSS columns для базового page flow;
- offscreen measurement container для chapter/section chunks;
- `Range.getClientRects()` для mapping text ranges;
- block-level pagination с уточнением на text range boundary;
- lazy page-map generation по мере чтения;
- cache `PageMap` по `DocumentRevision + ReaderSettings + viewport bucket`.

Точный web-алгоритм принят в
[`../adr/0006-browser-measured-pagination.md`](../adr/0006-browser-measured-pagination.md):
инкрементальное наполнение page-sized measurement container, browser layout и
binary search по text range с source-backed `PageBoundary`. Он проверяется на
длинном тексте, таблице, изображении и сноске в
[`../visuals/pagination-spike/`](../visuals/pagination-spike/).

## Anchors And Selection

Selection создается platform adapter, но anchor создает reader core.

Поток:

1. Пользователь выделяет текст на rendered surface.
2. Adapter получает selection/ranges через platform APIs.
3. Adapter мапит DOM/native nodes в `ReadingNode` через stable data attributes.
4. Reader core создает `Anchor`: node path, offsets, quote, prefix/suffix,
   content hash, source map и revision.
5. Annotation/highlight/note хранится поверх anchor.

Важное правило: DOM path не является primary anchor. DOM нужен только как
текущая визуальная проекция `ReadingDocument`.

## Overlays

Overlays должны быть независимы от source format:

- highlights;
- notes;
- margin notes;
- bookmarks;
- shared comments;
- learning prompts;
- AI result markers;
- search matches.

Для reflowable reader overlays строятся по `Anchor -> current layout rects`.
Для fixed-layout/PDF overlays строятся по `page/rect + optional text anchor`.

Overlay layer должен переживать:

- смену темы;
- смену размера шрифта;
- поворот экрана;
- desktop window resize;
- переход между paginated и optional scroll mode;
- повторный импорт документа с восстановлением anchors.

## Elements And Plugin Blocks

Базовые `ReadingNode`:

- document;
- section/chapter;
- heading;
- paragraph;
- text span;
- link;
- list;
- blockquote;
- figure/image;
- caption;
- table;
- code block;
- footnote/endnote;
- callout;
- divider;
- exercise placeholder;
- plugin block placeholder.

First-party plugin blocks:

- code highlighting;
- MathML/LaTeX;
- Mermaid/diagrams;
- SVG;
- media overlays;
- `lum` interactive blocks;
- future fixed-layout helpers.

Plugin block contract:

- получает typed input из `ReadingDocument`;
- не получает прямой доступ к raw source file без capability;
- возвращает renderable output и anchor mapping;
- сообщает размер/measurement hints для pagination;
- работает offline, если все resources локальны;
- имеет explicit security policy.

## Fixed-Layout And PDF Surfaces

PDF и fixed-layout EPUB являются исключениями из reflowable reader.

Для них используется `PageFidelityDocument` поверх fixed-layout normalized
package и page fidelity surface:

```text
Page fidelity surface
  -> visual page layer
  -> text layer
  -> annotation overlay layer
  -> search/AI/export layer
```

Правила:

- оригинальная раскладка сохраняется;
- progress, annotations, search, timeline и AI context остаются общими;
- anchors хранят и координатную, и текстовую привязку, когда возможно;
- fixed-layout EPUB имеет кнопку перехода в normalized mode;
- normalized mode использует тот же `ReadingDocument`, но может терять fidelity.

## Кроссплатформенность

Кроссплатформенность строится не на попытке идеально одинаково отрисовать
пиксели, а на одинаковых contracts:

- один Normalized Content Package contract для imported revisions;
- один `ReadingDocument`;
- один `PageFidelityDocument` для fixed-layout/PDF;
- один reader domain core;
- один anchor model;
- один annotation model;
- один timeline event model;
- один plugin contract;
- разные platform adapters.

Платформенные различия допустимы только на уровне:

- layout measurements;
- gesture handling;
- panel placement;
- file/permission APIs;
- native DRM bridges;
- audio recording/playback;
- performance tuning.

Запрещено:

- хранить anchors в platform-specific DOM paths;
- держать разные модели заметок на web/mobile/desktop;
- использовать EPUB-specific renderer как основной path только на одной
  платформе;
- делать plugin API, завязанный только на web DOM;
- смешивать platform UI events с reader core state.

## Технический стек

Базовый выбор:

- Rust для importer, reader core, models, anchors, sync-friendly domain logic.
- Dioxus 0.7 как основной UI framework-кандидат для web/desktop/mobile.
- Dioxus Web для первого web target.
- Dioxus Desktop/WebView для desktop target, если подтвердится качество reader
  surface.
- Dioxus Mobile/WebView для mobile target, с возможностью native shell fallback.
- Browser/WebView layout engine для typography, selection и accessibility.
- Axum/SQLx для server-side и persistence слоев вне reader UI.

Важная оговорка: Dioxus и Dioxus Mobile еще нужно валидировать reader-прототипом.
Если mobile reader, DRM, selection или performance упрутся в ограничения, core
остается Rust/shared, а platform adapter может быть заменен.

## Альтернативы

- `rejected`: полностью свой layout/typesetting engine. Слишком дорого:
  Unicode, bidi, hyphenation, font fallback, selection и accessibility не должны
  становиться задачей Lumi.
- `rejected`: отдельный EPUB/PDF/web renderer как основной path для каждого
  формата. Это ломает единые anchors, notes, search, learning и ИИ-контекст.
- `rejected`: native text renderer на каждой платформе как основной reader.
  Это даст разные selection/layout semantics и усложнит синхронизацию anchors.
- `revisit`: Paged.js/Vivliostyle для web pagination. Можно использовать как
  adapter-level dependency, но нельзя завязывать reader core на web-only
  pagination engine.
- `revisit`: Readium как renderer для EPUB/fixed-layout/mobile DRM. Readium
  полезен как reference или specialized adapter, но не должен становиться
  единственным reader contract.
- `revisit`: Tauri shell для desktop/mobile. Может быть fallback/alternative
  shell, если Dioxus Desktop/Mobile окажутся недостаточными.

## Открытые вопросы

- Какие browser/WebView-specific invalidation и performance tuning нужны
  принятому ADR 0006 на desktop/mobile targets?
- Можно ли на Dioxus Mobile получить достаточно надежный text selection и
  geometry measurement для качественных highlights?
- Нужен ли отдельный native bridge для Android/iOS DRM, или достаточно
  shared Rust/Readium-compatible provider?
- Какой fixed-layout renderer безопаснее: sandboxed WebView/iframe,
  rasterization, собственный constrained renderer или Readium-like adapter?
- Какие plugin blocks можно выполнять в основном reader surface, а какие нужно
  изолировать в sandbox?

## Источники

- [Dioxus 0.7 Introduction](https://dioxuslabs.com/learn/0.7/)
- [Dioxus Web](https://dioxuslabs.com/learn/0.7/guides/platforms/web/)
- [Dioxus Desktop](https://dioxuslabs.com/learn/0.7/guides/platforms/desktop/)
- [Dioxus Mobile](https://dioxuslabs.com/learn/0.7/guides/platforms/mobile/)
- [Tauri](https://tauri.app/)
