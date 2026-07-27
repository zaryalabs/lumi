# Desk projection

Status: accepted

Runbook относится к `0.4.0/E4` и
[`ADR 0034`](../adr/0034-desk-projection-and-search-surfaces.md).

## Проверка состояния

Desk — derived projection над primary materials, annotations, learning и
accepted AI artifacts. Авторизованный запрос первой страницы:

```sh
curl -sS -b cookies.txt \
  'http://127.0.0.1:8080/api/v1/desk/materials?limit=30'
```

Ответ содержит `projection_version`, `projection_generation`, bounded items и
optional cursor. Detail и cross-material срезы:

```sh
curl -sS -b cookies.txt \
  'http://127.0.0.1:8080/api/v1/desk/items?type=annotation&sort=updated&limit=30'
curl -sS -b cookies.txt \
  "http://127.0.0.1:8080/api/v1/desk/materials/$MATERIAL_ID"
```

Проверять содержимое чужого account вручную нельзя. Для диагностики достаточно
owner id, projection version/generation, counters и object ids; note bodies,
transcripts и artifact payload в telemetry не пишутся.

## Полный rebuild

Rebuild не изменяет primary objects:

```sh
curl -sS -X POST -b cookies.txt \
  -H "x-lumi-csrf: $LUMI_CSRF" \
  http://127.0.0.1:8080/api/v1/desk/rebuild
```

Команда удаляет только owner-scoped projection rows, повторно строит их из
активных материалов, Annotation v2, learning state и accepted AI artifacts и
возвращает новую generation.

Rebuild нужен после:

- изменения `DESK_PROJECTION_VERSION`;
- восстановления primary PostgreSQL без derived rows;
- подтверждённого расхождения counters/items;
- несовместимой миграции projection filters или sort keys.

## Диагностика расхождения

1. Сохранить material/item response и текущую generation.
2. Проверить, что primary object активен и принадлежит тому же `user_id`.
3. Проверить projection rows без чтения payload:

```sql
SELECT object_type, count(*), max(updated_at)
FROM desk_item_projection
WHERE user_id = $1
GROUP BY object_type;
```

4. Выполнить authorized rebuild.
5. Сравнить object ids и counters до/после. Если расхождение повторяется,
   проверить domain trigger в той же транзакции, а не исправлять projection
   строку вручную.

## Rollback

Desk можно временно скрыть на уровне capability/navigation, не отключая
Reader, annotation CRUD, learning, AI или search. Таблицы projection допустимо
очистить только owner-scoped rebuild-командой. Primary `materials`,
`annotations`, `learning_*`, `ai_artifacts` и их revisions удалять нельзя.
