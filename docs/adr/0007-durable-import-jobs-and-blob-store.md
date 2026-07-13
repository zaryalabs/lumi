# ADR 0007: долговечные import jobs и content-addressed blob store

Status: accepted

## Контекст

Этап 2 принимает недоверенный EPUB через HTTP, сохраняет исходник до запуска
парсера и должен завершить или диагностируемо восстановить импорт после падения
процесса. Нельзя держать upload, состояние worker и результат только в памяти.
При этом local development использует файловую систему, а production direction
остаётся S3-compatible object storage без изменения доменной модели.

## Решение

- `BlobStore` является асинхронным contract по content hash. Backend обязан
  проверить SHA-256 при записи и чтении; domain records хранят hash, размер,
  media type, backend и opaque storage key.
- Local backend пишет в `sha256/<prefix>/<hash>` через временный файл и atomic
  rename. Корень задаётся `LUMI_BLOB_ROOT`; путь пользователя не участвует в
  filesystem key.
- Upload сначала сохраняет source blob, затем одной PostgreSQL-транзакцией
  создаёт `Material`, queued `import_job`, idempotency response и sync change.
- Worker атомарно claim-ит queued job, увеличивает `attempt` и проходит
  cooperative cancellation checkpoints. Он не публикует частичный revision.
- Успешная публикация одной транзакцией создаёт immutable
  `DocumentRevision`, Normalized Content Package, source map, blob manifest,
  diagnostics, обновляет active revision/material state и завершает job.
- Ошибка или отмена сохраняет stable diagnostic и material-level import state.
  Source blob остаётся доступен для retry и диагностики.
- На startup `running` jobs без запроса отмены возвращаются в очередь. Jobs с
  исчерпанным retry budget завершаются `epub_retry_exhausted`.
- Файлы, записанные в blob backend до неуспешного database commit, считаются
  безопасными content-addressed orphans и удаляются будущей retention/GC policy.

## Последствия

- Restart не теряет upload и не требует повторной отправки файла.
- PostgreSQL остаётся system of record для lifecycle и ownership, blob backend —
  для immutable bytes.
- Local filesystem backend пригоден только для одного локального deployment.
  Multi-instance production должен предоставить S3-compatible реализацию того
  же `BlobStore` contract и lifecycle bucket policy.
- Retry одного job сохраняет diagnostics предыдущих attempts; UI получает их в
  порядке от нового attempt к старому.

## Альтернативы

- Хранить source bytes в `import_jobs.source_ref`: отклонено из-за размера,
  backup amplification и невозможности перейти на object storage.
- Публиковать revision по мере разбора spine: отклонено, потому что crash
  оставлял бы частично читаемый material.
- Использовать только in-memory queue: отклонено, потому что queued/running
  work терялся бы при deploy или restart.
- Удалять source сразу после failed import: отклонено, потому что retry требовал
  бы повторного upload и diagnostics теряли provenance.

## Compatibility

- Forward-only migration
  `20260713130000_stage2_real_epub_import.sql` расширяет Stage 1 schema.
- Старый fixture-backed router остаётся только для unit/API scaffold; persistent
  process использует durable import service.
- Integration checks обязаны покрывать success, malformed failure, owner scope,
  restart recovery, cancel/retry и content hash verification.

