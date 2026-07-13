# API-backed библиотека: локальная проверка

Status: active

Runbook описывает Этап 3 `S1 Web Reader`: authoritative PostgreSQL
projection библиотеки, lifecycle материала и Dioxus UI без fixture state.

## Запуск

```sh
make db-up
make db-migrate
make server-r
make web-r
```

Открыть `http://127.0.0.1:5173`, создать аккаунт или войти по recovery phrase.
Новый аккаунт получает честный empty state. После выбора EPUB материал сразу
появляется как `queued`/`importing`, затем переходит в `ready` или `failed` без
локальной подмены данными.

## HTTP contract

- `GET /api/v1/materials` — все active и archived материалы аккаунта, кроме
  tombstones;
- `GET /api/v1/materials/{material_id}` — сведения одного материала;
- `PATCH /api/v1/materials/{material_id}/library-state` — archive/restore;
- `DELETE /api/v1/materials/{material_id}` — soft delete и sync tombstone;
- `GET /api/v1/materials/{material_id}/source` — исходный EPUB с безопасным
  `Content-Disposition`;
- `/api/v1/imports` и `/api/v1/jobs/*` сохраняют совместимость с job lifecycle
  Этапа 2.

Mutation requests требуют session cookie, exact Origin/Referer,
`X-Lumi-CSRF` и непустой `Idempotency-Key` длиной не более 200 символов.
Повтор lifecycle-команды с тем же ключом и payload не создаёт второй sync
change. Повтор того же ключа с другим object/payload возвращает `409 Conflict`.

`LibraryEntry.active_revision_id` отсутствует до успешной публикации, поэтому
pending и failed material являются полноценными библиотечными состояниями, а
не частично созданными `Material`. `latest_job` содержит stage и structured
diagnostics для карточки и details dialog.

## Archive и delete

Archive меняет `library_state` между `active` и `archived`; исходник, job и
revision сохраняются. Delete является soft delete: сервер выставляет
`library_state=deleted`, `deleted_at`, увеличивает object revision и добавляет
`delete` change. Material сразу исчезает из list/details/source routes.
Физическое удаление PostgreSQL records и content-addressed blobs относится к
будущей retention/GC policy.

## Проверка persistence и isolation

1. Импортировать поддерживаемый и повреждённый EPUB.
2. Открыть сведения, скачать исходник, архивировать и восстановить готовый
   материал, удалить повреждённый.
3. Перезагрузить browser и перезапустить server с теми же `DATABASE_URL` и
   `LUMI_BLOB_ROOT` — состояния должны сохраниться.
4. Войти вторым аккаунтом — список должен быть пуст, а прямые id первого
   аккаунта должны возвращать `404`.

Автоматическая проверка:

```sh
make c
make web-e2e
```

Playwright journey дополнительно сужает viewport до mobile-размера и проверяет,
что карточка остаётся доступной через semantic role/label.

Если стандартный API port занят, browser stack можно изолировать без остановки
другого процесса:

```sh
LUMI_E2E_API_PORT=18080 make web-e2e
```
