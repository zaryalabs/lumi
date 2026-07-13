# Durable annotations и progress

## Назначение

Runbook описывает локальную проверку Этапа 5 S1: browser Selection → полный
source-backed anchor → durable highlight/note → overlay/panel/export.

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

1. Выделите фрагмент мышью, клавиатурой или touch selection и создайте
   highlight.
2. Создайте note, откройте панель «Заметки», перейдите к цитате и отредактируйте
   текст.
3. Измените тему, размер и ширину: overlay должен остаться на той же цитате, а
   позиция — на том же Unicode scalar boundary.
4. Перезагрузите browser и server: position, highlight и note должны остаться.
5. Откройте тот же note в двух окнах. После stale edit UI должен показать
   conflict, загрузить server revision и сохранить локальный draft отдельно.
6. Скачайте export: `lumi-annotations-<material-id>.json` содержит schema marker,
   provenance, quote, note body, timestamps и полный anchor.
7. Удалите annotation. Она исчезает из panel/export, но остаётся PostgreSQL
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
