# Application shell v2

Status: accepted

Date: 2026-08-02

## Задача и словарь

Shell группирует действия по пользовательской задаче и оставляет Reader
иммерсивной рабочей поверхностью. В интерфейсе используются названия
`Повторение`, `Пространства`, `Активность` и `ИИ`; `capability`, provider,
revision, score и другие детали реализации показываются только в диагностике
или явных расширенных настройках.

Локальный фильтр Библиотеки работает только по уже загруженным материалам.
Глобальный Поиск — отдельное действие app bar и ищет также в записях,
обучении, ИИ-результатах и доступных социальных источниках.

## Карта навигации

| Уровень | Desktop/tablet | Mobile |
| --- | --- | --- |
| Primary | Библиотека, Desk, Пространства, Повторение | нижняя панель с теми же четырьмя пунктами |
| Utility | Поиск, Активность | контекстная верхняя панель и прямые routes |
| Account | Настройки, Подключения, установка PWA, выход | меню аватара |
| Admin | отдельная permission-gated ссылка | меню аккаунта |
| Reader | собственные Поиск, Оглавление, Заметки, Ещё | компактный нижний dock из четырёх действий |

`#ai-queue`, `#connections`, `#admin` и прежние hash routes остаются прямыми и
reload-safe. Смена route переводит фокус в `#main-content`.

## Responsive tiers

- `> 980px`: полный desktop app bar;
- `761–980px`: compact app bar, подписи вторичных действий могут скрываться;
- `<= 760px`: contextual top bar и fixed bottom navigation;
- Reader не изменяет ширину страницы при открытии панели: desktop panel —
  overlay, mobile panel — bottom sheet;
- safe-area inset применяется к верхним и нижним fixed surfaces;
- минимальный viewport — `320 CSS px`, touch target — не меньше `44 CSS px`.

## Semantic tokens

| Семантика | Tokens |
| --- | --- |
| Canvas/surfaces | `--canvas`, `--surface-1`, `--surface-2`, `--surface-inset` |
| Text/border | `--text-primary`, `--text-secondary`, `--border-subtle`, `--border-strong` |
| Accent/status | `--accent-primary`, `--accent-hover`, `--accent-subtle`, `--status-*` |
| Interaction | `--focus-ring`, `--selection`, `--radius-*`, `--elevation-*` |
| Layers | `--layer-sticky`, `--layer-popover`, `--layer-sheet`, `--layer-toast`, `--layer-assistant`, `--layer-modal` |

UI использует системный sans-serif stack, а reading content — Georgia/serif.
Необъявленные feature-local custom properties запрещены и проверяются
`make web-css-l`.

## Overlay contract

Порядок слоёв: sticky chrome → popover → sheet/drawer → toast → assistant →
modal. Одновременно Reader открывает только одну основную panel. Открытие
возвращает фокус на close control, закрытие — на trigger; Escape закрывает
верхний contextual layer. Настоящие modal flows используют native
`dialog.showModal()`. Desktop Reader panels документированы как overlay и не
участвуют в PageMap geometry. Fixed ИИ-trigger скрывается при Reader, dialog,
scrim и selection composer.

Popover ограничен viewport по высоте и получает внутренний scroll. На mobile
material/Reader menus становятся bottom sheets и открываются выше safe-area.

## Reader

Основной toolbar содержит Поиск, Оглавление, Заметки и Ещё. Настройки,
запись на полях, голос, Community, export и summary находятся в contextual
слое. Notes реализует ARIA tabs с roving `tabindex` и Arrow/Home/End.

PageMap измеряет фактический stage и computed Reader styles, слушает
`ResizeObserver`, `visualViewport.resize` и window resize с debounce `160 ms`,
а после перестройки восстанавливает source `PageBoundary`. Оглавление рендерит
не больше 160 ближайших/отфильтрованных строк.

## Состояния и acceptance

Loading, empty, partial, error и ready не показываются одновременно. Ошибка
всегда предлагает bounded recovery (`Повторить`, сбросить фильтры или вернуться
к источнику). Paper/night покрывают panels, dialogs, composers и ИИ-handoff.

Интерактивный acceptance artifact находится в
[`prototype/`](prototype/) и проверяется `make prototype-e2e`.
