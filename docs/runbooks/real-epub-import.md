# Real EPUB import: локальный запуск и проверка

Status: active

Runbook описывает Этап 2 `S1 Web Reader`: multipart upload, local blob
backend, долговечный worker, безопасную нормализацию EPUB и диагностику ошибок.

## Запуск

```sh
make db-up
make db-migrate
make server-r
make web-r
```

По умолчанию immutable source/resources/packages сохраняются в
`.local/blob-store`. Корень можно изменить до запуска server:

```sh
LUMI_BLOB_ROOT=/absolute/path/to/lumi-blobs make server-r
```

Каталог входит в `.local/` и не коммитится. Удаление PostgreSQL без удаления
blob root или наоборот создаёт неполное local state; для чистого старта нужно
очищать оба слоя осознанно.

## HTTP contract

Upload:

```text
POST /api/v1/imports
Content-Type: multipart/form-data
Idempotency-Key: <1..200 chars>
X-Lumi-CSRF: <session csrf>

file=<DRM-free reflowable .epub>
```

Ответ `202 Accepted` содержит `material_id` и durable `job`. Доступные routes:

- `GET /api/v1/imports` — совместимый alias списка состояний импорта;
- `GET /api/v1/materials` — authoritative projection библиотеки;
- `GET /api/v1/jobs/{job_id}` — status/stage/result ids;
- `GET /api/v1/jobs/{job_id}/diagnostics` — stable diagnostics;
- `POST /api/v1/jobs/{job_id}/cancel` — cooperative cancellation;
- `POST /api/v1/jobs/{job_id}/retry` — повтор failed/cancelled job;
- `GET /api/v1/materials/{material_id}/source` — исходный EPUB;
- `GET /api/v1/revisions/{revision_id}` — immutable revision metadata;
- `GET /api/v1/revisions/{revision_id}/package` — normalized package;
- `GET /api/v1/revisions/{revision_id}/reading-document` — reader projection;
- `GET /api/v1/blobs/{manifest_id}` — scoped manifest без blob bytes.

Все routes требуют session и owner scope; mutation дополнительно требует exact
Origin/Referer и CSRF. Другой аккаунт получает `404`/не видит object.

## Lifecycle и recovery

```text
queued/source_accepted
  -> running/validating_container
  -> running/normalizing
  -> running/persisting
  -> succeeded/committed
```

Parser или security error переводит job в `failed`, cancellation — в
`cancelled`. Failed material остаётся в `GET /imports` с diagnostic code и
может быть повторён без нового upload.

После аварийного завершения запустить тот же server с прежними `DATABASE_URL`
и `LUMI_BLOB_ROOT`. Startup recovery вернёт незавершённый `running` job в
очередь. Максимум попыток хранится в PostgreSQL; после исчерпания появляется
`epub_retry_exhausted`.

## Security profile

Importer применяет `epub-limits.s1`: source 100 MiB, 10 000 ZIP entries,
512 MiB expanded aggregate, 64 MiB на resource, 2 MiB на package XML/NCX,
8 MiB на XHTML, path 1024 bytes и compression ratio 100:1.

Отклоняются path traversal, duplicate paths, symlinks, ZIP encryption,
неподдерживаемая compression, DTD/DOCTYPE, fixed-layout и locked publications.
XHTML проходит whitelist normalization; scripts, event handlers, iframes,
remote resources и EPUB CSS не становятся reader markup.

## Проверка

```sh
make c
make web-e2e
```

Browser flow проверяет registration, supported real EPUB, malformed ZIP,
diagnostic state и browser reload. Для ручной restart-проверки загрузить EPUB,
остановить `make server-r` во время/после импорта, снова запустить server и
убедиться, что job/material/source/package сохранились.
