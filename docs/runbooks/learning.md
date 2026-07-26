# Learning: повторение, AI и explain-back

## Назначение

Runbook описывает repository-side проверку learning verticals `0.3.0/E1–E3`.
Voice/transcription сюда не входят.

## Capability и маршруты

Готовый persistent server публикует:

- route group `learning`;
- `learning-core`;
- `learning-scheduling`.
- `learning-ai` и `learning-explain-back`, когда доступны общие AI provider,
  queue и explicit-context prerequisites.

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
```

`due_at` хранится в UTC. Paused schedules не должны попадать в due projection.
При ошибке concurrent submit проверяются `learning_mutations`, unique
`(session_id, item_id)` и schedule row revision; ослаблять эти ограничения
нельзя.
