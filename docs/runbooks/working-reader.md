# Рабочий web-reader

Этап 4 открывает готовый EPUB из API-backed библиотеки через hash-route
`#reader/<material_id>`. Reader получает только typed `ReadingDocument`; исходные
EPUB XHTML/CSS не являются product model и не монтируются в UI.

## Локальный запуск

```sh
make db-up
make db-migrate
make server-r
make web-r
```

Зарегистрируйтесь, загрузите DRM-free reflowable EPUB и нажмите «Читать» на
готовой карточке. Прямая ссылка reader сохраняется в browser hash и переживает
reload при действующей сессии.

## Что проверить вручную

1. Перейдите вперёд и назад по страницам длинной главы.
2. Откройте оглавление, перейдите в другой раздел и используйте history назад.
3. Откройте reader-native сноску и вернитесь в текст.
4. Измените тему, размер текста и ширину строки: текущая source-backed позиция
   должна сохраниться после repagination.
5. Перезагрузите browser: тема и позиция должны восстановиться.
6. Повторите на viewport около `390x844`: панели становятся нижними sheets, а в
   активном DOM остаётся только текущая страница.

## PageMap

Web adapter создаёт скрытый page-sized measurement container, использует
browser layout и `Range`, а границу длинного текста ищет binary search по
Unicode scalar offsets. После измерения container удаляется. Карта проверяет
непрерывное half-open покрытие каждого render block и кешируется в памяти по:

```text
revision + viewport + ReaderSettings + adapter version
```

Page number не сохраняется. PostgreSQL получает `Anchor` с node path и scalar
offset, поэтому reload и смена типографики могут построить другую карту и всё
равно вернуть пользователя к тому же фрагменту.

## API

- `GET /api/v1/revisions/{revision_id}/reading-document`
- `GET /api/v1/revisions/{revision_id}/resources/{content_hash}`
- `GET|PUT /api/v1/reader/settings`
- `GET|PUT /api/v1/materials/{material_id}/progress`

Unsafe commands требуют session cookie, CSRF header и `Idempotency-Key`.

## Автоматическая проверка

```sh
make c
LUMI_E2E_API_PORT=18080 make web-e2e
```

Playwright собирает настоящий EPUB с двумя длинными главами, TOC, internal link
и footnote, затем проверяет pagination, history, settings, reload persistence,
mobile layout и прежний lifecycle библиотеки.
