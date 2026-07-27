# Голосовые записи и внутренние связи

Status: accepted

## Пользовательский контракт

- В Reader действие «Голос» доступно для selection и текущего блока.
- Микрофон запрашивается только после «Начать запись». До upload запись можно
  прослушать или удалить; при недоступном MediaRecorder можно выбрать WebM,
  OGG, M4A/MP4, MP3 или WAV до 25 МиБ.
- После reload playback идёт через owner-scoped
  `/api/v1/audio/attachments/{id}/audio`.
- Voice Note не запускает Whisper. Уже связанный transcript показывается как
  отдельный artifact.
- В Markdown note поддерживаются `[[цель]]`, `[[материал#заголовок]]` и
  `[[цель|alias]]`. Неоднозначная ссылка требует выбора, неразрешённая
  сохраняется.
- Notes panel показывает исходящие и обратные ссылки; stable target переживает
  изменение display title.

## Диагностика

1. Проверить capability `voice-notes`, `stable-link-targets`,
   `annotation-wikilinks`, `annotation-backlinks`.
2. Для upload проверить последовательность: reserve → PUT bytes → complete →
   generic attachment → annotation.
3. HTTP 400 на PUT обычно означает MIME/signature, checksum или size mismatch.
   Audio bytes и note body не писать в logs.
4. HTTP 404 на playback означает foreign/deleted attachment либо отсутствующий
   blob. Hash не использовать для обхода authorization.
5. HTTP 409 на delete attachment означает active learning/annotation reference.
6. Для ссылок проверить `annotation_links.state`. `ambiguous` исправляется через
   explicit resolve; `unresolved` не является потерей note.
7. Backlinks не имеют отдельного authoritative body. При расхождении их можно
   перестроить owner-scoped `POST /api/v1/links/rebuild`: команда повторно
   разбирает note bodies, не изменяя primary annotation.

## Backup/restore

Backup должен включать PostgreSQL `audio_uploads`, `audio_attachments`,
`transcript_artifacts`, `annotation_links`, annotations и content-addressed blob
root. После restore:

1. проверить owner-scoped playback и byte range;
2. сравнить attachment checksum с blob;
3. открыть resolved/unresolved/ambiguous links;
4. проверить backlinks и portable export;
5. не запускать physical orphan cleanup до завершения проверки references.

## Ограничения

- JSON export содержит audio manifest, но не raw bytes.
- Автоматическая транскрибация Voice Notes не входит в `0.4.0/E2`.
- Physical orphan blob GC остаётся отдельной bounded operator/background
  операцией.
