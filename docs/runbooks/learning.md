# Learning: повторение, AI и explain-back

## Назначение

Runbook описывает repository-side проверку learning verticals `0.3.0/E1–E4` и
platform/release slice `0.3.0/E5`.

## Capability и маршруты

### Голосовые ответы

Persistent server публикует foundation `learning-audio-attachments` и
использует общий bounded
attachment flow: reserve через `POST /api/v1/blobs/uploads`, передача bytes
через `PUT /api/v1/blobs/uploads/{id}` и explicit complete. Затем
`POST /api/v1/learning/attachments` связывает blob с session item, а
`POST .../{id}/transcribe` создаёт durable transcript revision.

Встроенный provider использует отдельный account-scoped OpenAI credential,
который хранится зашифрованно через `PUT /api/v1/providers/openai/credential`.
Browser не получает key и не обращается в OpenAI напрямую. После provider result
пользователь редактирует текст и вызывает
`POST .../{id}/transcript/accept`. До acceptance grading запрещён.
`DELETE .../{id}/audio` закрывает original download, не удаляя transcript.
Допустимы WebM, Ogg, M4A/MP4, MP3 и WAV до 25 MiB. При ошибке permission,
credential или provider обычный text input остаётся доступным.
Повтор `POST .../{id}/transcribe` с новым idempotency key создаёт новую
transcript revision для того же attachment; повтор с тем же key возвращает
прежний результат.

Готовый persistent server публикует:

- route group `learning`;
- `learning-core`;
- `learning-scheduling`;
- `learning-ai` и `learning-explain-back`, когда доступны общие AI provider,
  queue и explicit-context prerequisites;
- `learning-audio-attachments`;
- `learning-voice`;
- `learning-mcp`; AI-dependent `create_flashcard_task` появляется в
  `tools/list` только вместе с `learning-ai`.

Проверка:

```sh
curl -fsS http://127.0.0.1:8080/api/v1/capabilities
```

Основной Web route — `#challenges`. Он показывает отдельно due, ready и draft
counts, а payload `Сегодня` всегда ограничен account daily limit.

## Локальная проверка

```sh
make db-up
make db-migrate
make l
make t
make web-e2e
```

Ручной journey:

1. Завершить scope в Reader и создать/активировать hinted question.
2. Пройти первую сессию, раскрыть hint или открыть source и выбрать review
   rating.
3. Открыть `#challenges`: attempt должен иметь assistance evidence, schedule —
   `algorithm_version=fsrs-4.5-lumi-v1`.
4. Поставить material на паузу. Его due items и overdue count должны исчезнуть,
   ручная практика и история — остаться.
5. Возобновить material: `Сегодня` остаётся bounded daily limit, старый backlog
   не раскрывается целиком.
6. В scheduled session выбрать «Отложить на завтра»: unanswered items получают
   новый `due_at`, attempts/failures не создаются.
7. Включить manual-only: due count сохраняется, автоматическая очередь
   скрывается, ручная практика работает.

AI journey:

1. Настроить OpenRouter BYOK в настройках аккаунта.
2. На странице learning материала нажать `Создать тест с AI`.
3. Проверить общую очередь AI: task использует
   `generate_learning_items`/`question-set-artifact.v1`.
4. После завершения обновить страницу материала: generated items имеют
   `Черновик`; отредактировать и явно активировать один из них.
5. Для открытого вопроса в session ввести ответ и нажать
   `Проверить ответ с AI`. Пока task выполняется, self-check остаётся
   доступным.
6. Обновить обратную связь. `understood/partial/needs_review` должны содержать
   citation ids; `not_evaluated` объясняется как отсутствие оценки, а не как
   ошибка пользователя.
7. Для активного `explain_back_prompt` запустить `Объяснить своими словами`,
   повторить feedback turn и убедиться, что предыдущие evaluation rows не
   перезаписаны.

Без provider шаги deterministic learning продолжают работать. AI task получает
provider failure/needs-input state; attempt не становится incorrect.

Voice journey:

1. В открытом или explain-back задании нажать `Записать ответ` и разрешить
   микрофон. Проверить timer, остановку, preview и удаление до upload.
2. Отправить запись без OpenAI key: transcript получает безопасный `failed`,
   text fallback остаётся доступным.
3. Сохранить отдельный OpenAI key и нажать `Повторить транскрибацию`: новая
   revision создаётся для того же attachment без повторной записи.
4. Исправить распознанный текст, подтвердить transcript и только после этого
   отправить ответ на self-check или AI evaluation.
5. При retention `delete_after_transcript` original audio больше не скачивается,
   accepted transcript сохраняется.

## Local fake provider

Обычный `make web-e2e` сам запускает
`tests/e2e/openrouter-mock.mjs` и направляет сервер на его localhost endpoint.
Для ручной сессии mock можно поднять отдельно:

```sh
LUMI_E2E_OPENROUTER_PORT=19090 node tests/e2e/openrouter-mock.mjs
LUMI_OPENROUTER_ENDPOINT=http://127.0.0.1:19090/api/v1/chat/completions \
LUMI_OPENAI_TRANSCRIPTION_ENDPOINT=http://127.0.0.1:19090/v1/audio/transcriptions \
make server-r
```

Mock принимает только тестовый credential и возвращает bounded deterministic
responses. Он не должен использоваться вне local/E2E окружения.

## MCP learning smoke

Создайте и скопируйте account token по
[`mcp-external-agents.md`](mcp-external-agents.md), затем проверьте registry:

```sh
curl -sS http://127.0.0.1:8080/mcp \
  -H 'Authorization: Bearer lumi_mcp_REDACTED' \
  -H 'Content-Type: application/json' \
  -H 'MCP-Protocol-Version: 2025-06-18' \
  --data '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}'
```

`list_learning_items` и `submit_learning_answer` используют owner-scoped
`LearningRuntime`; повтор submit с тем же idempotency key возвращает тот же
attempt. `create_flashcard_task` использует общий
`generate_learning_items`/`question-set-artifact.v1` task, а не отдельный MCP
worker. Foreign-account ids возвращают typed `not_found`; provider credentials,
raw audio и conversation runtime через MCP не выдаются.

## Восстановление после ошибок

- `rate_limited`/HTTP `429`: дождаться следующего минутного окна; не менять key
  при безопасном повторе той же mutation.
- `unavailable`/provider timeout: deterministic session остаётся доступной;
  AI task проверяется в общей очереди и повторяется штатным retry.
- transcription failure: сохранить attachment, оставить text fallback и
  повторить transcription после восстановления provider; grading до accepted
  transcript не запускать.
- stale/concurrent attempt: перечитать session; уникальность
  `(session_id, item_id)` и mutation key не обходить.
- после отзыва MCP token любые повторы выполняются только новым connection.

## Диагностика PostgreSQL

```sql
SELECT item_id, state, due_at, stability, difficulty,
       algorithm_version, repetitions, lapses, paused_at
FROM learning_schedules
ORDER BY due_at, item_id;

SELECT event_kind, payload, created_at
FROM learning_attempt_events
WHERE event_kind IN ('hint_revealed', 'source_opened')
ORDER BY created_at;

SELECT li.item_id, li.status, li.origin, provenance.task_id,
       provenance.artifact_id, provenance.citation_ids
FROM learning_items li
JOIN learning_generated_item_provenance provenance USING (item_id)
ORDER BY li.created_at;

SELECT session_id, item_id, task_id, artifact_id, payload, created_at
FROM learning_ai_evaluations
ORDER BY created_at;

SELECT attachment.id, attachment.owner_id, ref.session_id, ref.item_id,
       attachment.retention, attachment.audio_deleted_at, attachment.created_at
FROM audio_attachments attachment
JOIN learning_attachment_refs ref ON ref.attachment_id = attachment.id
ORDER BY attachment.created_at;

SELECT id, attachment_id, revision, status, provider, model, accepted_at
FROM transcript_artifacts
ORDER BY attachment_id, revision;
```

`due_at` хранится в UTC. Paused schedules не должны попадать в due projection.
При ошибке concurrent submit проверяются `learning_mutations`, unique
`(session_id, item_id)` и schedule row revision; ослаблять эти ограничения
нельзя.
