# ADR 0041: Web PWA shell, safe static cache и exact routes

Status: accepted

Date: 2026-08-02

## Контекст

Web-клиенту нужен installable app shell, но browser не является полной
локальной репликой. Кеширование API или owner data до отдельного offline-data
design удерживало бы приватные данные после logout/account switch. Одновременно
старые hash routes теряли typed origin и сворачивали search target до строки
node path.

## Решение

### PWA

- root-scoped `/service-worker.js` управляет versioned shell/runtime caches;
- precache ограничен публичными shell, manifest, icons и offline fallback;
- public static assets могут попадать только в rebuildable runtime cache;
- `/api/v1`, auth, source, audio и mutable owner data всегда `NetworkOnly`;
- offline fallback сообщает, что приватные материалы требуют сети, и не
  обещает offline-ready библиотеку;
- logout/session expiry отправляет `CLEAR_ACCOUNT_STATE` и очищает runtime
  cache;
- новая версия показывает уведомление. `SKIP_WAITING` и reload происходят
  только по действию пользователя и не во время Reader или dirty form;
- incompatible release меняет cache version; rollback возвращает предыдущие
  public assets и service worker с новой version string.

### Exact routes

`AppRoute::Reader` сериализует bounded `Anchor` как percent-encoded JSON и
typed `ReaderOrigin`. `LearningSession` хранит `LearningOrigin`, а social route
— `CommunityTarget` с shared material/source или message identity. Старый
reader route с `return_to` читается как compatibility input.

Anchor остаётся source-backed и проходит обычную server/Reader validation. Он
не является DOM path и не расширяет права доступа.

### Overlay и theme

Shell и Reader используют единый layer scale из
[`application-shell-v2.md`](../visuals/application-shell-v2.md). Theme color
синхронизируется с shell/Reader theme, viewport использует
`viewport-fit=cover`, fixed surfaces учитывают safe areas.

## Последствия

- PWA устанавливается и открывает static shell offline без копии owner data;
- настоящее offline reading остаётся отдельным будущим решением;
- exact Search/Community/Learning links стали длиннее, но reload-safe и
  сохраняют семантическую цель;
- обновление не может самопроизвольно потерять позицию чтения или draft;
- deployment обязан отдавать manifest, icons и service worker без auth и не
  кешировать service worker как immutable asset.

## Альтернативы

- CacheFirst для API и источников отклонён из-за privacy/staleness boundary.
- Хранить только последний `node_path` отклонено: теряется text range, PDF
  locator и social identity.
- Немедленный `skipWaiting` отклонён из-за риска потерять Reader position и
  несохранённую форму.

## Совместимость

- compatibility tests покрывают старые и новые hash routes;
- browser gate проверяет manifest, cache denylist, controlled update и offline
  fallback;
- rollback описан в
  [`pwa-release-and-rollback.md`](../runbooks/pwa-release-and-rollback.md).
