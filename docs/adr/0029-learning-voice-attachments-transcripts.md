# ADR 0029: голосовые ответы, attachments и transcript review

Status: `accepted`

Дата: 2026-07-26

## Контекст

Голосовой ответ не должен создавать второй learning attempt или обходить
обычную проверку ответа. Исходное аудио содержит персональные данные, а
provider transcript может быть ошибочным, поэтому grading до явного review
недопустим.

## Решение

1. `AudioAttachment` является общим owner-scoped attachment поверх
   content-addressed blob storage. Learning хранит только ссылку на attachment,
   session и immutable item.
2. Upload выполняется тремя bounded шагами: reserve metadata, передача точного
   тела, complete после SHA-256 и length validation. Разрешены только
   `audio/webm`, `audio/ogg`, `audio/mp4`, `audio/mpeg` и WAV; предел — 25 MiB.
3. `TranscriptArtifact` append-only и versioned. Provider result получает
   `needs_review`; пользовательская правка создаёт новую `accepted` revision.
   Только accepted text может быть передан в обычный grading/explain-back.
4. Исходное аудио и transcript имеют независимый lifecycle. Logical delete
   немедленно закрывает download, но сохраняет принятый transcript и learning
   history. Политика `delete_after_transcript` применяется после acceptance.
5. Все операции проверяют владельца. Сырые audio/transcript не попадают в
   logs, analytics и search.
6. Встроенный provider остаётся OpenAI Whisper `whisper-1` по ADR 0025;
   credential account-scoped и не заменяется OpenRouter key.

## Последствия

- отсутствие microphone, записи или provider не блокирует обычный text input;
- retry transcription создаёт новую transcript revision, но не learning attempt;
- удалённое аудио недоступно по старой attachment-ссылке;
- будущие Voice Notes используют тот же generic attachment lifecycle.
