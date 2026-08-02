# Выпуск и rollback Web PWA

Status: accepted

## Локальная проверка

1. Выполнить `make web-build` и убедиться, что в public output есть
   `manifest.webmanifest`, `service-worker.js`, `offline.html`, `pwa.js` и
   четыре варианта icons.
2. Выполнить `make web-e2e` и проверить проекты desktop Chromium, mobile
   Chromium, iPhone WebKit и tablet WebKit, если browser stack установлен.
3. В DevTools Application проверить scope `/`, отсутствие `/api/v1`, source и
   audio responses в Cache Storage.
4. Перевести браузер offline: должен открыться только static shell/fallback с
   честным сообщением о необходимости сети.
5. Открыть Reader и dirty form, установить waiting worker: обновление не должно
   перезагружать страницу без явного подтверждения.

## Выпуск

- при несовместимом изменении увеличить `VERSION` в `service-worker.js`;
- сначала опубликовать согласованный набор HTML/JS/WASM/CSS/icons, затем
  service worker;
- не назначать service worker immutable cache headers;
- staging smoke должен получить manifest/icons/SW без auth и подтвердить
  `Service-Worker-Allowed: /` либо эквивалентный root scope.

## Rollback

1. Вернуть предыдущий согласованный набор public assets.
2. Выпустить rollback service worker с **новым** `VERSION`, чтобы activate
   удалил ошибочные versioned caches.
3. Не переиспользовать имя неудачного cache: clients могли сохранить его.
4. Проверить controlled update и offline fallback в новой вкладке и уже
   контролируемом client.
5. Если worker не активируется, временно выпустить unregister worker, который
   удаляет только caches с префиксом `lumi-static-`, затем восстановить
   исправленный worker.

Logout/account switch дополнительно отправляет `CLEAR_ACCOUNT_STATE`; это не
заменяет version cleanup при rollback.
