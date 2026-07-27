# Durable annotations и progress

## Назначение

Runbook описывает локальную проверку Records v2 и Rich Reader:
browser Selection или margin action → полный source-backed target/anchor →
durable highlight/note → overlay/panel/export.

## Запуск

```sh
docker compose up -d --wait postgres
make db-migrate
make server-r
make web-r
```

Откройте `http://127.0.0.1:5173`, зарегистрируйтесь, импортируйте EPUB и
откройте reader.

## Ручная проверка

1. Выделите фрагмент мышью, клавиатурой или touch selection и создайте жёлтый
   highlight. В панели смените стиль на жирный: границы страницы не должны
   измениться.
2. Создайте note с title/tags, откройте панель «Заметки», перейдите к цитате,
   отредактируйте текст и проверьте фильтры `Все`/`Заметки`/`Выделения`.
3. Без выделения нажмите `Запись на полях`, создайте заметку только
   клавиатурой и проверьте block target в reflowable reader и page target в
   PDF.
4. Измените тему, размер и ширину: overlay должен остаться на той же цитате, а
   позиция — на том же Unicode scalar boundary.
5. Перезагрузите browser и server: position, highlight и note должны остаться.
6. Откройте тот же note в двух окнах. После stale edit UI должен показать
   conflict, загрузить server revision и сохранить локальный draft отдельно.
7. Скачайте export: `lumi-annotations-<material-id>.json` содержит marker
   `lumi.annotations.v2`, provenance, target/type, title/tags/status, payload,
   timestamps и полный anchor.
8. Удалите annotation. Она исчезает из panel/export, но остаётся PostgreSQL
   tombstone и `delete` change.

## Диагностика

- `400 annotation anchor does not match persisted normalized content` — client
  прислал path/offset/quote/hash, не совпадающие с active revision.
- `409` — reused idempotency key с другим command или stale
  `expected_revision`.
- «Не сохранено» в reader — durable ack не получен; retry annotation повторяет
  тот же `Idempotency-Key`.
- `Unresolved` — recovery ladder не нашёл единственный достаточно надёжный
  target; данные anchor не удаляются.

## Проверки

```sh
make c
make web-e2e
```

Web E2E использует локальный PostgreSQL и проверяет create/edit/delete,
reload, repagination, mobile notes sheet и portable export.
