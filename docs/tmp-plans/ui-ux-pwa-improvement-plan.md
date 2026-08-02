# План улучшения UI/UX и PWA Lumi

Status: `completed`

Последнее обновление: 2026-08-02

## Итог реализации

План реализован 2026-08-02. Durable-решения перенесены в канонические
product/system docs, визуальный контракт
[`application-shell-v2.md`](../visuals/application-shell-v2.md),
[`ADR-0041`](../adr/0041-web-pwa-shell-and-exact-routes.md) и
[`PWA release/rollback runbook`](../runbooks/pwa-release-and-rollback.md).
Продолжение оформлено в [`POST-0.5.0-ROADMAP.md`](POST-0.5.0-ROADMAP.md).

Release evidence:

- `make c` — пройден;
- `make web-e2e` — `27 passed`, `3 skipped`; ожидаемо пропущены live-profile
  smoke и Cache Storage checks в WebKit, остальные Chromium, Pixel, iPhone и
  iPad проекты пройдены;
- `make prototype-e2e` — `4 passed`;
- `make web-build` — production Web/PWA assets собраны;
- baseline сохранён в разделе «Фактический baseline», а воспроизводимый
  visual evidence — в расширенном static prototype и browser-проверках
  semantic/computed styles. Pixel snapshots не используются как release gate,
  поскольку для текущей кроссплатформенной матрицы они нестабильнее
  детерминированных DOM/CSS-инвариантов.

## Назначение

Этот документ превращает аудит текущего Web UI в последовательный план
реализации. План закрывает системные причины визуальной плоскости,
перегруженную информационную архитектуру, responsive-дефекты, проблемы
ключевых пользовательских переходов и отсутствие PWA-контракта.

План не является каноническим источником продуктовых или архитектурных
решений: итоговые контракты shell, навигации, offline-кеширования и
пользовательских настроек перенесены в соответствующие документы под `docs/`
и закреплены ADR. Завершённый [`ROADMAP.md`](ROADMAP.md) не переоткрыт;
дальнейшие задачи вынесены в отдельный post-`0.5.0` roadmap.

## Канонические источники

План реализует и уточняет требования из следующих документов:

- [Vision](../vision.md) — основной цикл чтения, User Space, рабочие
  поверхности и платформенное направление;
- [Экран чтения](../systems/reading-screen.md) — Reader, панели, обучение,
  социальные слои, offline-first и доступность;
- [Архитектура Reader](../systems/reader-architecture.md) — PageMap,
  platform adapters и границы reflowable/PDF;
- [Web account](../systems/web-account.md) — account shell и web-клиент;
- [Desk](../systems/desk.md) — рабочая поверхность записей и результатов;
- [Search](../systems/search.md) — единый поиск, exact open targets и social
  search;
- [Learning](../systems/learning.md) — ручной и completion-driven вход в
  обучение;
- [Social](../systems/social.md) — User Space, Community Space и совместное
  чтение;
- [AI chat](../systems/ai-chat.md) — глобальный persistent assistant;
- [Security & privacy](../systems/security-privacy.md) — account scope,
  sensitive data и browser boundary;
- [Quality](../systems/quality.md) — release gates, ADR и кроссплатформенное
  качество;
- [Reader-first визуальное направление](../visuals/reader-first-direction.md)
  — paper/sage, content-first Reader, desktop drawers и mobile sheets.

## Фактический baseline

На момент составления плана Web-клиент функционально покрывает Library,
Reader, Desk, Search, Learning, Community Spaces, AI queue, Connections и
глобальный AI chat. Аудит выявил следующие системные разрывы.

### Визуальная система

- В `apps/web/assets/main.css` определён один набор root tokens, но Desk,
  Search и Community используют необъявленные `--font-display`, `--ink-soft`,
  `--sage`, `--paper-raised`, `--paper`, `--border`, `--sage-strong` и другие
  переменные. Браузер отбрасывает соответствующие declarations.
- Один глобальный CSS-файл обслуживает все feature surfaces; общего слоя
  `Button`, `Field`, `Select`, `Surface`, `StatusPill` и overlay primitives нет.
- Library/Challenges, Desk/Search и Learning используют разные
  типографические рецепты. Указанный в stack `Inter` не подключён.
- Status modifiers `ready` и `succeeded` используются в markup, но не имеют
  полного визуального контракта.
- Night theme Reader не покрывает все dialogs, contextual composers и AI UI.

### Shell и responsive

- В primary navigation одновременно находятся Library, Desk, Search,
  Communities, Challenges, AI Queue, Connections и optional Admin.
- На mobile навигация превращается в горизонтальную строку без overflow cue и
  занимает вместе с account row около `143px` до основного контента.
- На ширине `768px` desktop shell переполняет viewport: навигация и account
  actions обрезаются, а `body { overflow-x: hidden; }` маскирует проблему.
- В приложении нет отдельного Profile/Settings hub. AI/provider, Telegram,
  MCP, Reader и Learning settings распределены по разным поверхностям.

### Reader

- В toolbar одинаковый вес имеют Search, TOC, Settings, Margin Note, Voice
  Note, Notes, Community и More.
- На mobile действия сжимаются в горизонтальную строку с многострочными
  подписями; notes tabs переносятся на две строки.
- Fixed AI trigger конкурирует с selection actions, sheets, notes и карточками
  за нижний правый угол.
- Material/Reader popovers не гарантируют collision-aware placement и могут
  открываться за нижнюю границу viewport.
- PageMap строится по `window.innerWidth/innerHeight`, не подписан на
  resize/orientation/VisualViewport и использует padding, отличающийся от
  реального mobile CSS.
- Большое оглавление целиком рендерится в DOM; на проверенном материале это
  `467` интерактивных строк.
- PDF selection в browser adapter зависит от `mouseup` и не покрывает touch и
  keyboard selection.

### Ключевые сценарии

- Library action `Учиться` открывает material learning без `source_id`; при
  отсутствии заданий пользователь попадает в тупик.
- Global Search не отображает social source types, а Reader open target теряет
  полный Anchor и сворачивается до последнего `node_path`.
- Matched Community material не предлагает открыть собственную копию в
  Reader.
- Reader return action имеет label `Вернуться в библиотеку`, даже если origin
  — Learning Session; завершение session также не всегда сохраняет origin.
- Loading, empty и error иногда визуально конкурируют или не предлагают
  recovery action.
- Пользовательский copy смешивает русский язык с `Durable execution`,
  `reload`, `permission-aware`, `BYOK`, `provider`, `capability`, raw revision и
  search score.

### PWA и platform contract

- Нет Web App Manifest, app icons, `apple-touch-icon`, service worker,
  install/update lifecycle и offline app shell.
- `theme-color` статичен и не синхронизируется с night theme.
- Safe-area declarations присутствуют фрагментарно, но viewport не использует
  `viewport-fit=cover`, а fullscreen AI chat не имеет полного inset contract.
- Основной Playwright release config проверяет desktop Chromium; нет полной
  матрицы WebKit/iPhone, tablet, landscape, PWA install/offline/update и mobile
  PDF.

## Целевой результат

После завершения плана Lumi должен восприниматься как единое приложение, а не
набор последовательно добавленных web-страниц.

Обязательный пользовательский outcome:

1. Основные сценарии доступны через компактную, предсказуемую навигацию на
   desktop, tablet и mobile.
2. Все controls и surfaces используют один semantic design contract и имеют
   полную state matrix.
3. Reader остаётся content-first: chrome не перекрывает текст, а secondary
   actions открываются контекстно.
4. Library → Reader → Learning → Source → Return, Search → exact target и
   Community → personal Reader проходят без тупиков и потери origin.
5. Web-клиент устанавливается как PWA, имеет корректный app shell, safe areas,
   controlled update flow и безопасный offline fallback.
6. Критические состояния проверяются автоматизированно на desktop Chromium,
   mobile Chromium, WebKit/iPhone и tablet viewport.

## Scope

### Входит

- semantic design tokens и общие UI primitives;
- унификация typography, spacing, elevation, radii и interaction states;
- новый application shell и информационная архитектура primary/secondary
  navigation;
- mobile bottom navigation, tablet layout и desktop shell;
- общий Settings/Profile entry point без переноса admin permissions;
- исправление перечисленных broken journeys Search/Learning/Community;
- Reader toolbar, panels, popovers, responsive PageMap и AI-trigger policy;
- accessibility hardening и touch-target baseline;
- installable PWA baseline, safe-area contract и static offline app shell;
- visual, browser, accessibility и PWA release gates;
- обновление канонической документации и визуального прототипа.

### Не входит

- нативные Android, iOS, Desktop или Tauri-клиенты;
- полноценная local-first/full-copy replica;
- offline-кеширование пользовательских материалов, source blobs, аудио,
  приватных записей или API responses;
- новый sync protocol или изменение server ownership модели;
- редизайн backend domain contracts, не требуемый для исправления exact target,
  learning source и Community open flow;
- Knowledge Base, marketplace, public Spaces, новая AI subscription model;
- новый brand identity или маркетинговый landing page;
- функциональное расширение Library tags, если для него потребуется новая
  domain model. В этом плане допускаются только фильтры/сортировка по уже
  существующим данным и ясное разделение library/global search.

## Инварианты реализации

1. Reader core не получает зависимости от DOM/WebView/platform handles.
2. Существующие reload-safe hash routes сохраняются или получают явные
   совместимые redirects; сохранённые ссылки не ломаются молча.
3. Presentation-only изменения не требуют нового server capability flag.
4. Новая навигация не скрывает capability-gated feature без доступного
   contextual entry point.
5. Все destructive actions используют общий confirm/undo contract; немедленное
   необратимое удаление без подтверждения запрещено.
6. Все интерактивные controls имеют минимум `44x44 CSS px`; для основных
   Android actions целевой размер — `48x48 CSS px`.
7. Любой modal/sheet/popover имеет определённые focus entry, focus trap или
   modeless contract, Escape/back behavior, focus return и collision policy.
8. На поддерживаемых viewport нет скрытого horizontal page overflow.
9. PWA service worker не кеширует auth, `/api/v1`, source, audio или owner data
   до отдельного offline-data design и threat review.
10. Paper/night theme применяются ко всему Reader workspace, включая panels,
    dialogs, composers и AI handoff UI.
11. Техническая диагностика остаётся доступной, но находится в explicit
    advanced disclosure и не конкурирует с пользовательскими действиями.
12. Каждая крупная UI surface имеет loading, empty, error, partial и ready
    states с релевантным recovery action.

## Решения, обязательные до основного кода

### Product и IA

- Зафиксировать primary navigation desktop/mobile и место Search, Spaces,
  Activity, Connections и Admin.
- Определить общий Settings/Profile hub и ownership Reader/Learning/AI/
  Integration settings.
- Подтвердить, что Community остаётся compact User Space entry/contextual
  surface или становится постоянным primary destination.
- Зафиксировать пользовательский словарь: `AI` или `ИИ`, `Повторение` или
  `Челленджи`, `Spaces` или `Сообщества`, допустимые технические термины.
- Определить границу Library search и Global Search.

Решения обновляют как минимум `docs/vision.md`, `docs/systems/social.md`,
`docs/systems/web-account.md`, `docs/systems/desk.md` и
`docs/systems/learning.md`.

### Visual contract

- Добавить `docs/visuals/application-shell-v2.md` с shell, navigation,
  responsive tiers, overlay layers, typography scale и semantic tokens.
- Обновить static prototype так, чтобы он покрывал не только Library/Reader,
  но также shell, Search/Desk, Settings/Activity и один Community state.
- Зафиксировать token names и запретить feature-local aliases без объявления в
  общем contract.

### Architecture и security

- Принять ADR для PWA install/offline/update contract: scope service worker,
  cache classes, versioning, rollback, account switch/logout и запрет
  owner-data caching в первом срезе.
- Зафиксировать serialized exact Reader target, если существующий hash route не
  может переносить полный Anchor безопасно и bounded.
- Определить общий overlay/modal/sheet lifecycle для Dioxus Web.

## Последовательность реализации

Работа разделена на шесть последовательных эпиков. Внутри каждого эпика Web,
tests, documentation и bounded spikes могут выполняться параллельно после
фиксации общего contract.

### E0. Contract freeze и проверяемый прототип

Результат: согласованы IA, visual contract, PWA boundary и acceptance matrix;
production CSS и component tree ещё не переписываются.

Задачи:

- [x] Зафиксировать navigation map для desktop, tablet и mobile.
- [x] Зафиксировать mapping старых routes в новый shell.
- [x] Описать account/settings/activity hierarchy.
- [x] Утвердить semantic token table и typography scale.
- [x] Описать overlay layers и collision rules.
- [x] Добавить PWA ADR с cache/security/update policy.
- [x] Определить exact Reader target serialization.
- [x] Обновить canonical product/system docs.
- [x] Расширить static prototype и добавить prototype E2E для shell, Reader и
  mobile sheets.
- [x] Сохранить baseline screenshots и измерения текущих проблем как release
  evidence.

Gate E0:

- продуктовые названия и navigation mapping не имеют открытых вариантов;
- PWA ADR принят;
- target prototype проходит `make prototype-e2e`;
- scope и non-goals согласованы;
- план переведён в `planned` и добавлен в новый roadmap.

### E1. Design foundation и UI primitives

Результат: все существующие поверхности используют один работающий visual
contract; визуальные дефекты больше не маскируют последующие IA-изменения.

#### E1.1 Tokens и CSS structure

- [x] Инвентаризировать используемые custom properties и удалить/заменить все
  undefined variables.
- [x] Ввести semantic tokens для canvas, surfaces, text, borders, accent,
  danger, success, focus, elevation и overlay layers.
- [x] Добавить aliases только на время контролируемой миграции и удалить их в
  конце E1.
- [x] Зафиксировать light/paper/night mappings без feature-specific color
  islands.
- [x] Разделить CSS на tokens/base/components/features с одним production
  entrypoint и без нового runtime-загрузчика.
- [x] Добавить автоматическую проверку undefined CSS custom properties.

#### E1.2 Primitives

- [x] Выделить общие `Button`, `IconButton`, `Field`, `Select`, `Checkbox`,
  `Surface/Card`, `StatusPill`, `FeedbackState`, `Dialog`, `Sheet`, `Popover`
  и `Tabs` contracts.
- [x] Реализовать state matrix: default, hover, active, focus-visible,
  disabled, loading, selected и destructive.
- [x] Унифицировать min size, padding, radii, label/hint/error placement и
  async submit state.
- [x] Заменить нативно выглядящие Community, Search, AI Queue и note controls.
- [x] Свести delete flows Library/Reader/Community к общему contract.

#### E1.3 Typography и content hierarchy

- [x] Выбрать и фактически подключить UI font либо удалить фиктивный `Inter`
  dependency из stack.
- [x] Оставить книжный serif для reading content и ограниченного editorial
  display, а не для произвольных feature headings.
- [x] Зафиксировать responsive scale H1–H3, body, label, caption и numeric
  values.
- [x] Развести визуальные уровни canvas, section, card, inset и feedback;
  одинаковая карточка не должна обозначать все уровни сразу.
- [x] Перенести raw IDs, hashes, revision и diagnostics в advanced disclosure.

Gate E1:

- в computed styles нет undefined token-dependent declarations;
- все интерактивные controls соответствуют size/state contract;
- Library, Desk, Search, Community, Challenges, AI Queue и Connections
  визуально используют одну систему;
- paper/night screenshots не содержат theme islands;
- keyboard focus видим на `button`, `a`, `input`, `textarea`, `select`, tabs и
  custom summaries.

### E2. Application shell, IA и ключевые переходы

Результат: пользователь ориентируется по задачам, а не по внутренним
подсистемам; основные journeys не заканчиваются тупиками.

#### E2.1 Shell и navigation

Целевой baseline для прототипирования:

- desktop primary: `Библиотека`, `Desk`, `Повторение`, `Пространства`;
- Search — global app-bar action/command surface;
- AI Queue — `Activity` или AI hub, не primary destination;
- Connections, Profile, AI providers, Reader/Learning preferences — account
  menu/Settings;
- Admin — отдельная permission-gated area;
- mobile — bottom navigation максимум из четырёх постоянных destinations,
  contextual top app bar и account avatar/menu;
- Reader — immersive shell без глобальной navigation.

Окончательный mapping утверждается в E0.

Задачи:

- [x] Разделить primary, contextual, account и admin navigation.
- [x] Реализовать desktop/tablet/mobile shell без breakpoint gap.
- [x] Использовать content/container breakpoint вместо фиксированного `760px`,
  если фактическая вместимость требует более раннего compact mode.
- [x] Добавить active-item visibility и понятный overflow contract там, где
  scroll всё же остаётся.
- [x] Сохранить прямые reload-safe routes для AI Queue, Connections и Admin.
- [x] Обеспечить browser back/forward, deep link и focus management при смене
  route.
- [x] Добавить общий Settings/Profile entry point.

#### E2.2 Library, Desk, Search и Community IA

- [x] Ясно разделить local Library search и Global Search либо оставить один
  entry point с явным scope selector.
- [x] Добавить Library sorting/filtering по уже существующим данным.
- [x] Показать Telegram в Add Material flow как capability-aware источник или
  объяснить его место рядом с другими способами импорта.
- [x] В Community list сначала показывать memberships/empty state, а создание
  пространства переносить в primary action/modal.
- [x] В Community Space показывать материалы и обсуждение раньше управления,
  invites и members.
- [x] В Desk исправить семантику `Все записи`, сохранить filters/sort при
  list/detail navigation и показать доступные tag filters.

#### E2.3 Broken journeys

- [x] Передавать валидный learning source при ручном входе из material card или
  не показывать действие до доступности честного сценария.
- [x] Сохранять typed origin при переходах Challenges/Learning/Reader и
  подписывать back action фактическим destination.
- [x] Добавить social source groups в Global Search.
- [x] Передавать в Reader точный bounded Anchor, а не только последний
  `node_path`.
- [x] Открывать personal Reader из matched Community claim.
- [x] Сохранить shared material/message/highlight identity при открытии social
  search result.
- [x] Устранить ложные empty states и добавить retry/recovery в Challenges,
  Desk, Search, Reader resolver и claim picker.
- [x] Обновить устаревший Explain-back copy.
- [x] Убрать raw score, capability и implementation language из ordinary UX.

Gate E2:

- нет horizontal page overflow на `320`, `390`, `768`, `1024`, `1280` и
  `1440px`;
- primary navigation остаётся доступной при 200% zoom;
- старые direct routes продолжают открываться;
- Library → Reader → Learning → Source → Return проходит с сохранением origin;
- Search открывает exact personal и social target;
- Community claim открывает personal copy в Reader;
- loading/error/empty states не противоречат друг другу.

### E3. Reader workspace и contextual interaction

Результат: текст остаётся центральной поверхностью; Reader chrome не меняет
позицию пользователя и не перекрывает содержимое.

#### E3.1 Action hierarchy

- [x] Оставить в основном toolbar только TOC, Search и Notes или другой набор,
  утверждённый в E0.
- [x] Перенести Settings, Community, Export и Summary в contextual/More layer.
- [x] Создание highlight/note/voice/AI запускать из selection/placement context
  или одной компактной create action.
- [x] Реализовать отдельный mobile action dock без многострочных 44px labels.
- [x] Добавить collision-aware placement для More, footnote и contextual
  popovers.

#### E3.2 Overlay lifecycle

- [x] Ввести общую layer scale и единственный overlay coordinator.
- [x] Скрывать или безопасно перемещать global AI trigger при открытых modal,
  sheet, selection toolbar и composer.
- [x] Использовать native `showModal()` там, где UI действительно modal;
  modeless desktop panels должны иметь отдельный документированный contract.
- [x] Добавить focus entry, trap/modeless boundary, Escape/back и focus return
  для AI chat, Community dialogs и Reader panels.
- [x] Реализовать корректные ARIA tabs для Notes с roving tabindex и Arrow
  keys.

#### E3.3 Responsive pagination

- [x] Измерять реальный content box и computed styles Reader page.
- [x] Подписаться на `ResizeObserver` и `visualViewport.resize` с bounded
  debounce.
- [x] При resize, orientation и split-screen пересчитывать PageMap и
  восстанавливать текущий source boundary.
- [x] Не менять текущий page content из-за открытия desktop side panel;
  использовать reserved workspace или overlay strategy.
- [x] Гарантировать видимость pagination внутри `100dvh` с safe-area inset.
- [x] Виртуализировать большое TOC либо применять bounded rendering с поиском
  и сохранением keyboard navigation.

#### E3.4 Reflowable/PDF parity

- [x] Вынести общие annotation/social/voice item views из reflowable и PDF
  adapters.
- [x] Добавить `pointerup`, `selectionchange` и keyboard path для PDF
  selection.
- [x] Уважать `prefers-reduced-motion` при PDF navigation.
- [x] Покрыть paper/night theme всеми Reader panels, dialogs и composers.
- [x] Исправить packaging/path PDF.js так, чтобы обычная загрузка приложения не
  создавала console error.

Gate E3:

- Reader не теряет current anchor при resize/orientation/open panel;
- AI trigger не перекрывает текст или actions ни в одном покрытом состоянии;
- toolbar не требует горизонтального поиска нужного действия;
- TOC большого EPUB не создаёт unbounded interaction cost;
- reflowable и PDF имеют одинаковые пользовательские сущности и ожидаемую
  keyboard/touch доступность;
- paper/night visual regression проходит для всех contextual layers.

### E4. PWA platform и release hardening

Результат: Lumi устанавливается как самостоятельное Web-приложение и имеет
безопасный static offline shell без обещания полноценной offline-библиотеки.

#### E4.1 Installability

- [x] Добавить `/manifest.webmanifest` с `id`, `name`, `short_name`,
  `start_url`, `scope`, `display: standalone`, language, orientation policy,
  theme/background colors и shortcuts, если они подтверждены UX.
- [x] Добавить `192x192`, `512x512`, maskable icons, favicon и
  `apple-touch-icon`.
- [x] Добавить `viewport-fit=cover` и полный safe-area contract.
- [x] Синхронизировать `theme-color` с shell и Reader night theme.
- [x] Определить ненавязчивый install affordance и fallback на browser-native
  install UI.

#### E4.2 Service worker и offline shell

- [x] Реализовать root-scoped service worker с versioned caches.
- [x] Precache только public hashed HTML/CSS/JS/WASM/fonts/icons/PDF.js assets.
- [x] Использовать `NetworkOnly` для `/api/v1`, auth, source, audio и mutable
  account data.
- [x] Добавить честный offline fallback: shell открывается, unavailable
  surfaces объясняют необходимость сети и не показывают stale private data.
- [x] Очищать versioned caches при incompatible upgrade; account logout не
  должен оставлять owner payload в Cache Storage.
- [x] Реализовать controlled update notification без неожиданного reload во
  время чтения или несохранённой формы.
- [x] Добавить rollback/runbook для ошибочного service worker release.

#### E4.3 Platform quality

- [x] Проверить standalone mode, browser back gesture, Android system bars,
  iOS notch/home indicator, software keyboard и landscape.
- [x] Добавить online/offline status только там, где он меняет доступные
  действия.
- [x] Не маркировать материалы как offline-ready до появления отдельного
  owner-scoped storage design.
- [x] Обновить deployment/staging smoke для manifest, icons, SW scope и cache
  headers.

Gate E4:

- browser признаёт приложение installable на поддерживаемом Chromium path;
- manifest и icons доступны без auth;
- standalone launch открывает корректный route и shell;
- static app shell открывается offline, API surfaces показывают честный
  unavailable state;
- service worker не кеширует запрещённые request classes;
- update flow не теряет reading position или draft input;
- safe-area и keyboard tests проходят на Pixel/iPhone profiles.

### E5. Общий release gate и документация

Результат: улучшения защищены автоматизированной матрицей и перенесены из
временного плана в durable documentation.

Задачи:

- [x] Добавить visual regression для основных surfaces и overlay states.
- [x] Добавить computed-style test на undefined custom properties.
- [x] Добавить accessibility smoke: keyboard-only, focus order/return, labels,
  dialogs, tabs, reduced motion и 200% zoom.
- [x] Добавить responsive regression для mobile, tablet, landscape и split
  layouts.
- [x] Добавить install/offline/update PWA tests.
- [x] Добавить mobile PDF selection и Reader resize/orientation E2E.
- [x] Проверить отсутствие unexpected console errors.
- [x] Обновить runbooks локальной проверки и PWA release/rollback.
- [x] Перенести durable решения в canonical docs/ADR.
- [x] Зафиксировать release evidence и перевести план в `completed` либо
  архивировать после выпуска.

Gate E5:

- `make c` проходит;
- `make web-e2e` проходит для доступного browser stack;
- `make prototype-e2e` проходит, если prototype остаётся acceptance artifact;
- PWA, responsive, accessibility и visual gates проходят в CI/staging;
- все completion criteria ниже закрыты.

## Внутренняя карта workstreams

После E0 задачи внутри активного эпика можно делить на следующие независимые
workstreams.

### A. Design system

- semantic tokens и themes;
- CSS structure;
- primitives и interaction states;
- typography и visual regression fixtures.

### B. Shell и product UX

- navigation/account/settings/activity;
- Library/Desk/Search/Community IA;
- copy и state model;
- learning/search/community route fixes.

### C. Reader

- toolbar/action hierarchy;
- overlay coordinator;
- PageMap resize/orientation;
- reflowable/PDF parity и large TOC.

### D. PWA platform

- manifest/icons/meta;
- service worker/cache/update;
- safe areas/standalone/offline fallback;
- deployment и rollback.

### E. Quality и documentation

- Playwright matrix;
- accessibility/visual/PWA gates;
- prototype acceptance;
- canonical docs, ADR и runbooks.

Workstreams не должны одновременно изменять один и тот же shell/overlay
contract до merge общего E0/E1 foundation.

## Test matrix

### Viewports и платформенные состояния

Минимальный набор:

- `320x568` — minimum supported phone;
- `390x844` — primary mobile portrait;
- `844x390` — mobile landscape;
- `768x1024` — tablet portrait и текущий overflow regression;
- `1024x768` — tablet landscape/small desktop;
- `1280x900` — standard desktop;
- `1440x900` — wide desktop;
- 200% browser zoom;
- standalone PWA display mode;
- software keyboard open;
- safe-area emulation;
- `prefers-reduced-motion: reduce`;
- paper и night themes.

### Browser projects

- Desktop Chromium;
- mobile Chromium/Pixel profile;
- WebKit/iPhone profile;
- WebKit/iPad или эквивалентный tablet profile;
- optional Firefox smoke для standards regressions, если стоимость CI остаётся
  bounded.

### Обязательные browser flows

1. Sign in → Library → import modal → cancel.
2. Library → Reader → TOC/Search/Notes/Settings → return.
3. Reader resize/orientation → тот же source boundary.
4. Reader selection → highlight/note/voice/AI contextual actions.
5. Library → Learning → source → return to original session.
6. Search personal exact target и social exact target.
7. Community claim → personal Reader.
8. Global AI chat open/close/route change с focus return.
9. Empty/loading/error/retry для Library, Search, Desk, Challenges, Community
   и AI Queue.
10. PWA install → standalone launch → offline shell → online recovery →
    controlled update.

## Performance и UX budgets

- Никакая primary navigation не вызывает horizontal page overflow.
- Initial shell не зависит от загрузки feature data.
- PageMap resize recalculation имеет debounce и не запускает параллельные
  unbounded rebuilds.
- Открытие/закрытие Reader panel не меняет source boundary.
- Большие списки TOC/search/chat используют bounded rendering strategy.
- Все animations используют transform/opacity либо имеют обоснованное
  исключение; reduced-motion отключает non-essential motion.
- Нет layout reads/writes, чередующихся внутри render path.
- PWA precache не включает large content, CMaps целиком без budget review или
  owner data.
- Release build не содержит неожиданных console error/warning на основных
  маршрутах.

Конкретные численные budgets для WASM bundle, shell start, PageMap rebuild и
TOC должны быть зафиксированы в E0 на текущем representative corpus, чтобы не
создавать произвольные цели без baseline.

## Rollout

1. E1 tokens/primitives выпускаются без feature flag, но малыми совместимыми
   PR с visual snapshots.
2. Новый shell сначала сохраняет все старые routes и может включаться через
   repository-side UI switch на staging; server capability для него не нужен.
3. Broken journey fixes выпускаются до визуального удаления старых entry
   points, чтобы не скрыть единственный рабочий путь.
4. Reader resize/overlay изменения проходят отдельный EPUB/PDF regression gate.
5. Service worker сначала выпускается с static-only cache и conservative update
   policy; offline user data остаётся выключенным.
6. После staging acceptance legacy CSS aliases, старый shell markup и
   временный UI switch удаляются.

## Риски и меры

### Большой CSS refactor создаст широкие regressions

Меры: сначала token aliases и computed-style gate, затем миграция по
поверхностям; visual snapshots до удаления legacy styles.

### Новый shell сломает deep links и browser history

Меры: frozen route mapping в E0, compatibility tests для каждого старого hash,
отдельные back/forward/reload fixtures.

### Reader resize изменит pagination и прогресс

Меры: восстанавливать semantic source boundary, а не page index; тестировать
EPUB с длинным TOC, mixed blocks и изменением width/height во время чтения.

### Overlay coordinator усложнит focus behavior

Меры: единый primitive, documented modal/modeless states, keyboard E2E и запрет
feature-local z-index вне общей шкалы.

### Service worker раскроет или удержит приватные данные

Меры: static-only allowlist, `NetworkOnly` для API/auth/content, ADR/threat
review, cache inspection tests и rollback runbook.

### IA-рефакторинг разрастётся в новый продуктовый roadmap

Меры: сохранять non-goals; новые domain features и Knowledge Base выносить в
отдельные планы, а не добавлять в этот hardening slice.

## Критический путь

```text
E0 contract freeze
  -> E1 tokens, primitives и themes
  -> E2 shell, IA и broken journeys
  -> E3 Reader interaction и responsive pagination
  -> E4 PWA install/offline/update
  -> E5 release gate и canonical documentation
```

E2 broken journey fixes, которые не зависят от нового shell markup, можно
готовить параллельно с E1, но интегрировать только после freeze route/overlay
contracts.

## Критерии завершения

План считается выполненным, когда одновременно истинны следующие условия:

1. Все использованные CSS custom properties объявлены и проверяются
   автоматически.
2. Все основные surfaces используют общий token/primitive/state contract.
3. Primary navigation не смешивает product, operations, integrations и admin
   levels и работает на mobile/tablet/desktop без clipping.
4. Нет интерактивных touch targets меньше `44x44 CSS px`.
5. AI trigger, popovers, sheets и contextual actions не перекрывают друг друга
   и читаемый контент.
6. Reader сохраняет source boundary при resize, orientation и открытии panels.
7. Large TOC имеет bounded rendering strategy.
8. Learning, exact Search и Community-to-Reader journeys проходят end-to-end.
9. Loading/empty/error/partial states честны, различимы и дают recovery.
10. Пользовательский copy не раскрывает implementation terminology без
    advanced context.
11. PWA manifest, icons, standalone launch, static offline shell и controlled
    update работают на поддерживаемых browser paths.
12. Service worker не кеширует owner/auth/API/source/audio data.
13. Desktop Chromium, mobile Chromium, WebKit/iPhone и tablet regression gates
    проходят.
14. `make c`, обязательный `make web-e2e` и релевантный
    `make prototype-e2e` проходят.
15. Durable IA, PWA, Reader и visual decisions перенесены в canonical docs и
    ADR; временный план получил status `completed` и подготовлен к архивированию.
