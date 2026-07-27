# ADR 0031: Voice Note и общий audio lifecycle

Status: accepted

## Контекст

Learning voice answers уже ввели owner-scoped `AudioUpload`,
`AudioAttachment`, content-addressed blob storage, browser `MediaRecorder` и
транскрипты. Records v2 должен добавить голосовые заметки, не создавая второй
uploader, blob store или transcription lifecycle.

Learning-specific создание attachment требовало session/item и поэтому не
могло безопасно использоваться Reader. Кроме того, playback требует
авторизованный byte-range download, а удаление одной записи не должно удалять
audio, на которое ещё ссылается learning или другая запись.

## Решение

1. `POST/PUT/complete /api/v1/blobs/uploads` остаётся единым bounded upload
   protocol. До записи blob сервер проверяет длину, SHA-256, allowlist MIME и
   сигнатуру контейнера.
2. `POST /api/v1/audio/attachments` создаёт generic immutable attachment только
   из завершённого owner-scoped upload. Команда имеет idempotency key, retention
   и optional duration до 10 минут.
3. Старый `/learning/attachments` сохраняется и использует тот же
   `audio_attachments`; session/item reference остаётся отдельным ограничением
   learning.
4. `AnnotationKind::VoiceNote` хранит только `audio_attachment_id`, optional
   `transcript_artifact_id` и bounded waveform summary. Audio bytes не входят в
   JSON, sync change или logs.
5. Create/update annotation проверяет, что attachment активен и принадлежит
   владельцу. Voice annotation создаётся только после durable upload complete и
   attachment commit.
6. Download `/api/v1/audio/attachments/{id}/audio` проверяет owner, возвращает
   safe media type, `nosniff`, `inline`, `Accept-Ranges` и один bounded byte
   range. Hash и storage key не являются credential и не раскрываются.
7. Delete annotation ставит tombstone и логически удаляет audio только при
   отсутствии active annotation и learning references. Физический
   content-addressed blob удаляется отдельным retention/GC проходом; общий hash
   нельзя удалять по одной ссылке.
8. Voice Note не запускает транскрибацию автоматически. Reader только показывает
   уже связанный `TranscriptArtifact`.
9. Web использует общий recorder adapter learning/records. Микрофон
   запрашивается только по кнопке; доступны preview, cancel и file fallback.
10. Portable `lumi.annotations.v2` export содержит metadata/audio manifest с
    checksum и явным `audio_bytes_included: false`.

## Последствия

- Records и learning разделяют storage, authorization и media policy.
- Незавершённый upload не создаёт broken Voice Note.
- Логическое удаление безопасно для shared attachment, но физический GC остаётся
  асинхронной операторской задачей.
- S3-compatible backend сможет реализовать тот же contract без изменения
  Annotation v2.

## Альтернативы

- `rejected`: отдельная таблица/blob API для Voice Notes — создаёт второй
  lifecycle и расходящиеся security rules.
- `rejected`: base64 audio в annotation JSON — ломает limits, sync и export.
- `rejected`: считать content hash bearer token — раскрывает private media.
- `rejected`: автоматически транскрибировать каждую запись — относится к
  отдельной AI capability и требует явного provider consent.

## Совместимость

- Миграция additive: `duration_ms` nullable, старые learning attachments
  остаются валидными.
- Старые learning routes не меняются.
- Required tests: MIME spoofing, checksum/size, upload-before-annotation,
  owner isolation, byte ranges, reload/playback, shared refcount, tombstone и
  manifest-only export.

