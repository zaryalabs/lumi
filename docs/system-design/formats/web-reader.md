# Веб-страницы в режиме чтения

Status: draft

## Контекст

Веб-страницы - один из основных входных источников Lumi наряду с EPUB, FB2 и
PDF. Пользователь должен иметь возможность сохранить статью, блог-пост,
документацию, лонгрид или другой текстовый материал из браузера и читать его в
той же среде, что книги и собственные `lum`-материалы.

Этот документ описывает web page source, а не web-клиент Lumi. Для web-first
версии импорт веб-страницы все равно проходит через web-аккаунт: URL fetch,
browser capture или manual HTML становятся account-scoped import jobs, которые
создают обычный `Material` в облачной реплике пользователя.

Главное решение: веб-страница не получает отдельный browser-renderer внутри
Lumi. Веб-реализация отвечает за получение страницы, извлечение основного
материала, сохранение snapshot, ресурсов и source map. Результат импорта -
обычный `ReadingDocument`, который отображается общим reader contract из
[`../reader-architecture.md`](../reader-architecture.md).

Lumi не должен превращаться в браузер общего назначения и не должен переносить
произвольный HTML/CSS/JS сайта в reader. Сайт является недоверенным источником
данных. Reader получает очищенную, нормализованную, аннотируемую модель статьи:
заголовки, абзацы, списки, цитаты, изображения, таблицы, код, ссылки и
placeholder-блоки для сложных embeds.

Приоритет отладки для `v01`: статьи и лонгриды с популярных источников вроде
Medium, Substack и Habr, плюс общий extractor для обычных HTML-страниц.

## Пользовательские сценарии

- Пользователь добавляет URL вручную из Lumi.
- Пользователь сохраняет страницу из браузера через extension, share sheet или
  bookmarklet.
- Lumi извлекает title, author, site name, canonical URL, published/modified
  dates, description, cover image, language и основной текст.
- Пользователь читает веб-статью в том же визуальном стиле, что reflowable
  EPUB, FB2, Markdown и `lum`.
- Пользователь читает сохраненную статью offline, даже если исходный сайт
  изменился или недоступен.
- Пользователь переходит по оглавлению, заголовкам, внутренним якорям и внешним
  ссылкам.
- Пользователь видит изображения, подписи, таблицы, code blocks, callout-like
  блоки и важные embedded resources в безопасной форме.
- Пользователь делает хайлайты, заметки, записи на полях и voice notes с
  устойчивой привязкой к фрагментам статьи.
- Пользователь ищет по сохраненным веб-страницам и использует их как контекст
  для базы знаний, обучения и ИИ-действий.
- Пользователь повторно импортирует статью, если исходный URL изменился, и Lumi
  пытается восстановить anchors на новой revision.

## Функциональные требования

### Поддерживаемые источники

- Public HTTP/HTTPS URL.
- DOM snapshot из browser extension/share sheet для страниц, которые требуют
  логина, активного пользовательского состояния или JavaScript rendering.
- Manual HTML paste или импорт выбранного фрагмента страницы как fallback для
  сложных случаев.
- Локальные `file://`, internal network URLs и custom schemes не входят в
  обычный web-import path. Если они понадобятся, это отдельная capability с
  явным разрешением пользователя.

### Capture modes

#### URL fetch

URL fetch - основной путь для публичных страниц:

- нормализовать URL и сохранить исходный ввод пользователя;
- для web-first версии создать authenticated import job в web-аккаунте
  пользователя;
- пройти redirects с лимитом и сохранить redirect chain;
- прочитать `Content-Type`, charset hints и фактический HTML;
- не исполнять JavaScript;
- не передавать пользовательские cookies, tokens или browser credentials;
- не загружать внешние subresources автоматически, кроме явно выбранных
  ресурсов импорта вроде изображений;
- создать `WebPageSnapshot` из ответа сервера.

URL fetch подходит для обычных статей, блогов, документации и сайтов, где
основной HTML доступен без выполнения JS.

#### Browser capture

Browser capture нужен для страниц, где пользователь уже имеет доступ через свой
браузер или где основной контент появляется после client-side rendering:

- extension/share sheet получает текущий visible DOM или reader-safe article
  fragment;
- capture передает Lumi HTML snapshot, current URL, canonical URL, metadata,
  base URL и selected resources в account-scoped import job;
- importer не получает browser cookies и не должен уметь переиспользовать
  сессию пользователя;
- capture должен явно показывать пользователю, что именно сохраняется;
- scripts, event handlers и runtime state не сохраняются как исполняемый код.

Этот путь не предназначен для обхода paywall или авторизации. Он сохраняет
только тот контент, к которому пользователь уже получил доступ в своем браузере.

#### Manual selection

Если extractor не может надежно найти основной материал, пользователь может
сохранить выделенный фрагмент страницы. Такой материал получает source metadata
и помечается как `selection_capture`, чтобы не смешивать его с полной статьей.

### Fetch policy

- Поддерживать `http` и `https`, но по умолчанию предпочитать `https`.
- Ограничивать размер ответа, время соединения, количество redirects и общий
  объем скачанных данных.
- Отклонять redirects на unsupported schemes.
- На server-side fetch защищаться от SSRF: запрещать private/link-local/local
  address ranges, loopback, metadata endpoints, Unix sockets, localhost aliases
  и DNS rebinding.
- Не обходить robots, paywall, rate limits и access restrictions.
- Сохранять диагностические данные: final URL, status code, response headers,
  content type, charset, redirect chain и import issues.
- Повторные попытки fetch должны быть ограничены и не должны превращаться в
  crawler.

### Metadata extraction

Importer извлекает metadata из нескольких источников с приоритетами:

- canonical link;
- Open Graph и Twitter Card metadata;
- Schema.org `Article`, `BlogPosting`, `NewsArticle` и похожие JSON-LD blocks;
- HTML `title`, `meta[name=description]`, `meta[name=author]`;
- `article`, `main`, headings и site-specific adapter hints;
- URL и домен как fallback.

Сохраняемые поля:

- исходный URL;
- canonical URL;
- final URL после redirects;
- site name/domain;
- title;
- subtitle/description;
- author(s);
- publisher/source;
- language;
- published date;
- modified date;
- imported date;
- cover image;
- estimated reading time;
- tags/categories, если они явно есть в metadata.

Metadata неполного качества не должна ломать импорт. Минимальный fallback title
строится из heading, HTML title или URL.

### Извлечение основного контента

Основной extractor должен:

- найти article-like область страницы;
- удалить navigation, header, footer, sidebars, comments, ads, cookie banners,
  share widgets, related posts и tracking blocks;
- сохранить структуру heading hierarchy;
- сохранить порядок текста, изображений, таблиц и кода;
- не смешивать основной текст с рекомендациями, меню и комментариями;
- сохранять `ImportIssue`, если уверенность extraction низкая.

Источники сигналов:

- semantic elements: `article`, `main`, `section`, `header`;
- ARIA roles и microdata;
- Schema.org article body hints;
- heading density и text density;
- link density;
- repeated boilerplate patterns;
- site-specific adapter rules;
- visible DOM snapshot metadata из browser capture.

Для Medium, Substack и Habr нужны отдельные site adapters, если generic
extractor теряет заголовки, подписи, код, изображения или автора.

### Нормализация контента

HTML не рендерится напрямую. Extracted article DOM мапится в `ReadingDocument`.

Block mapping:

- `article/main/section` -> section/chapter `ReadingNode`.
- `h1`-`h6` -> heading with normalized level.
- `p` -> paragraph.
- `ul/ol/li` -> list nodes.
- `blockquote` -> blockquote.
- `pre/code` -> code block with optional language metadata.
- `figure/img/picture` -> figure/image with caption.
- `figcaption` -> caption.
- `table/thead/tbody/tr/th/td` -> table nodes.
- `hr` -> divider.
- `aside` -> callout, note или supplemental block, если он связан с текстом.
- `details/summary` -> collapsible-like block или static callout, в зависимости
  от reader capabilities.
- `iframe`, embeds, forms и interactive widgets -> plugin placeholder или
  unsupported block.

Inline mapping:

- `strong/b` -> strong mark.
- `em/i` -> emphasis mark.
- `code` -> inline code.
- `a` -> internal, external или resource link.
- `sub/sup` -> subscript/superscript marks.
- `mark` -> highlight-like semantic mark, но не пользовательский highlight.
- `abbr`, `time`, `cite`, `kbd`, `samp` -> semantic inline marks, где полезно.

Исходный CSS не переносится в reader. Можно использовать только ограниченные
семантические подсказки: language, direction, hidden/visible state,
code-language classes, table semantics, figure captions и heading hierarchy.

### Ссылки

- Relative URLs разрешаются относительно base URL snapshot.
- Внутренние якоря превращаются в reader links, если target есть в
  `ReadingDocument`.
- Внешние ссылки сохраняются как external links и открываются через reader
  policy.
- Ссылки на изображения и downloadable resources сохраняются как source links.
- `mailto:`, `tel:`, custom schemes и unsafe schemes не активируются внутри
  reader без явного действия пользователя.

### Ресурсы

- Изображения сохраняются как локальные resources материала, если они участвуют
  в основном тексте.
- `srcset`/`picture` нормализуются: importer выбирает разумный candidate для
  reader и может сохранить альтернативы как metadata.
- Lazy-loaded images из `data-src`, `data-original`, `srcset` и captured DOM
  должны поддерживаться best-effort.
- `alt`, caption, dimensions, MIME type, checksum и source URL сохраняются.
- Remote images не должны подгружаться reader-ом автоматически после импорта,
  если они не сохранены локально.
- Unsupported или oversized resources заменяются placeholder-ом и
  `ImportIssue`.
- Video/audio embeds сохраняются как media placeholder с source URL, thumbnail
  и metadata, но не исполняются как сторонний embed по умолчанию.

### Embeds и интерактивность

Произвольные embeds нельзя переносить в reader как исполняемый HTML.

Подход:

- YouTube/Vimeo/audio embeds -> media placeholder with external action.
- Tweets/X posts, Telegram embeds, GitHub gists -> placeholder или future
  first-party import/plugin path.
- CodePen, JSFiddle, arbitrary iframes -> unsupported interactive block unless
  explicit plugin exists.
- Forms, comments widgets, newsletter signup, trackers -> удаляются.
- MathML/LaTeX, Mermaid, SVG и code highlighting идут через стандартные
  first-party plugin blocks, если importer может выделить typed input.

### Навигация

- TOC строится из heading hierarchy основного контента.
- Если heading hierarchy плохая, importer строит shallow TOC из значимых
  разделов или не создает TOC.
- Внутренние anchors страницы сохраняются как navigation targets.
- Reader progress считается по normalized `ReadingDocument`, а не по scroll
  position исходной страницы.
- Original URL и canonical URL доступны из панели metadata.

### Повторный импорт и версии

Сохраненная веб-страница должна быть snapshot-based:

- каждый успешный импорт создает immutable `DocumentRevision`;
- snapshot хранит normalized article, source metadata, source map и локальные
  resources;
- исходный URL можно refetch/reimport вручную или по отдельной политике;
- новая версия не заменяет silently старую, если у пользователя есть anchors,
  заметки или прогресс;
- Anchor recovery использует общую модель: quote, prefix/suffix context,
  content hash, `ReadingNode` path и source map;
- UI должен уметь показать, что статья изменилась, и предложить миграцию
  anchors на новую revision.

### Ошибки и деградация

- Если fetch не удался, материал может сохраниться как failed import с URL и
  понятной ошибкой.
- Если main content не найден, importer сохраняет raw snapshot metadata и
  предлагает browser capture или manual selection.
- Если extractor уверен частично, импорт создается, но сохраняется warning.
- Если часть resources не загрузилась, текст статьи остается доступным.
- Если HTML malformed, parser должен восстановить дерево best-effort.
- Если текст почти полностью состоит из ссылок, navigation или комментариев,
  importer должен пометить низкое качество результата.

## Нефункциональные требования

- **Единый вид.** Веб-страницы всегда отображаются через общий reflowable reader
  contract, без исходного CSS сайта.
- **Offline-first.** После импорта текст, metadata и основные ресурсы доступны
  без сети.
- **Безопасность.** HTML, CSS, SVG, images и embeds являются недоверенным
  input. Нельзя исполнять scripts, inline handlers, foreign iframes и unsafe
  URLs внутри reader.
- **SSRF-защита.** Server-side fetch не должен иметь доступ к локальной сети,
  loopback, cloud metadata endpoints и внутренним сервисам.
- **Приватность.** URL, snapshots, заметки и прогресс не отправляются на сервер
  или ИИ без явного пользовательского сценария. В web-версии сам import job
  является таким сценарием и пишет данные в облачную реплику аккаунта.
- **Детерминированность.** Один snapshot при одинаковой версии extractor должен
  давать одинаковые `ReadingNode` ids, source map и anchors.
- **Диагностируемость.** Для отладки сохраняются extraction confidence, adapter
  name/version, source paths, skipped blocks и import issues.
- **Производительность.** Большие страницы и длинные статьи импортируются с
  лимитами памяти, ленивой обработкой ресурсов и bounded parsing.
- **Устойчивость к site drift.** Site adapters должны быть изолированными и
  тестируемыми на fixtures, чтобы изменения одного сайта не ломали общий
  importer.
- **Правовая осторожность.** Lumi не должен обходить paywalls и не должен
  публиковать чужие snapshots без явного действия пользователя и будущих правил
  sharing/copyright.

## Модель данных

```text
WebInput
  -> WebCaptureProvider | WebFetchProvider
  -> WebPageSnapshot
  -> WebMetadata
  -> WebArticleCandidate
  -> WebResource[]
  -> ReadingDocument
```

Формат-специфичные сущности:

- `WebInput` - URL, browser capture, manual HTML или selection capture.
- `WebFetchProvider` - безопасный HTTP fetch с redirect, timeout и SSRF policy.
- `WebCaptureProvider` - extension/share sheet/manual capture source.
- `WebPageSnapshot` - immutable HTML/text snapshot, final URL, headers,
  captured date, checksum и capture mode.
- `WebMetadata` - title, author, site, dates, language, canonical URL,
  description, cover и raw metadata blocks.
- `WebSiteAdapter` - optional extractor adapter for domain/site family.
- `WebArticleCandidate` - extracted main content DOM плюс confidence и reasons.
- `WebContentMap` - связь `ReadingNode` с source DOM path, selector hints,
  text ranges и original URL.
- `WebResource` - локальный resource id, source URL, MIME type, dimensions,
  checksum, load status.
- `WebLinkMap` - resolved internal anchors, external links и unsafe/unsupported
  links.
- `WebImportIssue` - warning/error с source path, severity и причиной.

Web-specific anchor source:

```text
WebAnchorSource {
  original_url
  canonical_url
  snapshot_id
  capture_mode
  adapter_id
  source_dom_path
  source_selector_hint
  heading_path
  text_offset_start
  text_offset_end
}
```

Primary anchor остается общей anchor-моделью Lumi: `ReadingNode` path, quote,
prefix/suffix context, content hash и `DocumentRevision`. Web-specific данные
нужны для диагностики, экспорта, deep links и восстановления после повторного
импорта.

## Реализация

### Pipeline импорта

1. Принять URL, browser capture, HTML paste или selection capture.
2. Создать `Material` с source kind `web_page`.
3. Нормализовать URL и выбрать capture/fetch provider.
4. Для URL fetch выполнить безопасный HTTP request с redirect и SSRF policy.
5. Для browser/manual capture принять HTML snapshot и associated metadata.
6. Определить charset и декодировать HTML.
7. Распарсить HTML через tolerant HTML parser.
8. Извлечь metadata: canonical, Open Graph, Schema.org, title, author, dates,
   language и cover.
9. Выбрать site adapter по URL/domain/metadata или generic extractor.
10. Найти main content candidate и оценить extraction confidence.
11. Удалить boilerplate, tracking, forms, comments и unsafe blocks.
12. Преобразовать разрешенный DOM в `ReadingNode`.
13. Переписать links и anchors во внутренние reader targets или external links.
14. Найти изображения и resources, скачать/сохранить их по resource policy.
15. Создать placeholders для unsupported embeds и oversized resources.
16. Построить TOC из heading hierarchy.
17. Создать `ReadingDocument`, `DocumentRevision`, source map и import issues.
18. Передать текстовые слои в поиск и будущие ИИ/learning pipelines.

### Выбор библиотек

Базовый Rust stack:

- `reqwest` - HTTP client для URL fetch path с timeout, redirect и headers
  policy.
- `url` - нормализация URL и разрешение relative links/resources.
- `scraper` / `html5ever` - tolerant HTML parsing и DOM traversal.
- `ammonia` - defense-in-depth sanitizer для HTML fragments, если фрагмент
  нужно сохранить или показать до полной нормализации.
- `serde_json` - разбор JSON-LD metadata.
- `mime` / lightweight sniffing - проверка resource content types.
- `image` - безопасное получение размеров и базовая проверка изображений, если
  нужно без полного декодирования в UI.

Readability algorithm нужен как часть generic extractor, но конкретный выбор
остается `revisit`: взять существующую Rust-библиотеку, портировать Mozilla
Readability-подход или написать свой extractor поверх `scraper`. Для `v01`
важнее иметь тестовые fixtures и понятный source map, чем идеально совпадать с
браузерным reader mode.

### Site adapters

Site adapter не является отдельным renderer. Он только помогает extractor-у
найти и нормализовать основной контент.

Контракт adapter:

```text
WebSiteAdapter {
  id
  matches(url, metadata) -> bool
  extract(document, context) -> WebArticleCandidate
  normalize(candidate) -> WebArticleCandidate
  fixtures
}
```

Правила:

- adapter возвращает обычный article candidate, который проходит общий
  sanitizer, resource pipeline и `ReadingDocument` mapping;
- adapter не может выполнять произвольный JS;
- adapter должен быть покрыт HTML fixtures для типовых страниц;
- adapter failures не должны ломать generic extractor fallback;
- adapter version сохраняется в `WebPageSnapshot` для диагностики.

### Browser extension / share sheet

Extension или share sheet нужны не для чтения, а для capture:

- получить current URL, title, selection и visible DOM;
- предложить сохранить полную страницу или выделенный фрагмент;
- отправить в Lumi snapshot без cookies/tokens;
- передать base URL и список важных resources;
- получить от Lumi статус импорта.

Для web-first версии нужно отдельно решить, будет ли capture поверх browser
extension, bookmarklet, share target или server URL import. Контракт importer
должен поддерживать все эти способы без изменения reader.

### Безопасность импорта

- Не исполнять scripts, inline event handlers и arbitrary iframes.
- Удалять `script`, `style`, `noscript` по политике extractor; `noscript` можно
  использовать только как текстовый fallback, если он действительно содержит
  article content.
- Фильтровать URL schemes и опасные attributes.
- SVG обрабатывать как недоверенный resource: sanitize, rasterize или отдавать
  в isolated first-party plugin.
- CSS не применять к reader surface.
- Images декодировать и хранить с лимитами размера и типа.
- Server fetch должен проверять IP после DNS resolution и после redirects.
- Не хранить secrets из browser capture.
- Не отправлять article text в внешние extraction APIs по умолчанию.

## Интеграции и зависимости

- **Reader.** Web importer выдает `ReadingDocument`; reader отвечает за
  paginated rendering, anchors, заметки, timeline events и панели.
- **Reader architecture.** Web path не зависит от Dioxus/WebView напрямую и не
  хранит DOM как primary model.
- **Поиск.** Web importer передает normalized article text, title, site,
  author, URL и headings в индекс текущего документа и глобальный индекс.
- **База знаний.** Заметки и хайлайты экспортируются с source: title, URL, site,
  author, date, section, quote и backlink в Lumi.
- **Obsidian.** Экспорт веб-заметок должен давать Markdown с canonical URL,
  retrieved date, quote blocks и wikilinks.
- **ИИ.** Reader передает ИИ-слою normalized text, metadata, source URL и anchor
  context. Web importer не вызывает ИИ сам.
- **Веб-аккаунт.** Web-first URL fetch/browser capture создают import jobs в
  [`../web-account.md`](../web-account.md); blobs/resources сохраняются через
  account blob storage policy.
- **Плагины.** Site adapters, custom source providers, embed handlers и
  specialized block normalizers могут быть plugin extension points. Они не
  должны обходить security policy и общую anchor-модель.
- **Синхронизация.** Синхронизируются `Material`, `DocumentRevision`, metadata,
  resources metadata, anchors, progress и annotations. Полные HTML snapshots и
  images могут синхронизироваться по storage policy.
- **Социальные функции.** Shared comments используют те же anchors, но sharing
  snapshot текста требует отдельной privacy/copyright policy.

## Альтернативы

- `rejected`: открывать исходный сайт внутри embedded browser как основной
  reader. Это ломает единый вид, offline-first, anchors, extraction и
  безопасность.
- `rejected`: рендерить sanitized HTML с исходными CSS сайта. Даже очищенный
  HTML оставляет нестабильную верстку, чужие class/style assumptions и слабую
  source map для заметок.
- `rejected`: конвертировать веб-страницы в Markdown как primary internal
  model. Markdown удобен для экспорта, но теряет таблицы, figures, source map,
  resource metadata и будущие plugin blocks.
- `rejected`: server-only fetch для всех страниц. Он не работает для login,
  dynamic rendering и user-specific pages, а также создает лишние privacy
  риски.
- `rejected`: выполнять JavaScript страницы по умолчанию. Это ухудшает
  безопасность, производительность и повторяемость импорта.
- `accepted`: snapshot-based import. Сохраненная статья является immutable
  revision, а refetch создает новую revision.
- `accepted`: generic extractor + site adapters. Общий extractor покрывает
  большинство страниц, adapters нужны для важных источников и site drift.
- `revisit`: headless browser worker для JS-heavy public pages. Может помочь с
  dynamic sites, но требует отдельной sandbox, resource limits и privacy policy.
- `revisit`: MHTML/WARC-like archive как дополнительный экспорт snapshot.
  Полезно для переносимости, но не должно становиться internal reader model.

## Открытые вопросы

- Какой capture surface выбрать первым для web-first версии: browser extension,
  bookmarklet, share target или только manual URL import?
- Должен ли первый web target поддерживать только server URL import или сразу
  browser capture/upload snapshot через web account?
- Какие site adapters обязательны для `v01`: Medium, Substack, Habr и какие еще
  источники?
- Нужен ли headless browser worker для публичных JS-heavy страниц или достаточно
  browser capture?
- Как хранить и синхронизировать images/snapshots: всегда локально, выборочно
  или по user storage policy?
- Какой threshold extraction confidence должен требовать подтверждения
  пользователя?
- Нужно ли сохранять comments/discussion thread как supplemental content или
  intentionally удалять их из reader mode?
- Как показывать пользователю diff между двумя revisions одной статьи?
- Какие ограничения нужны на sharing цитат и snapshots в социальных функциях?

## Источники

- [Mozilla Readability](https://github.com/mozilla/readability)
- [WHATWG HTML Living Standard](https://html.spec.whatwg.org/)
- [Open Graph protocol](https://ogp.me/)
- [Schema.org Article](https://schema.org/Article)
- [oEmbed](https://oembed.com/)
- [`reqwest` crate](https://docs.rs/reqwest/)
- [`scraper` crate](https://docs.rs/scraper/)
- [`ammonia` crate](https://docs.rs/ammonia/)
- [`url` crate](https://docs.rs/url/)
