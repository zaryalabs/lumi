# Learning: повторение и Challenges

## Назначение

Runbook описывает repository-side проверку deterministic learning scheduling
эпика `0.3.0/E2`. AI generation, explain-back и voice сюда не входят.

## Capability и маршруты

Готовый persistent server публикует:

- route group `learning`;
- `learning-core`;
- `learning-scheduling`.

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
```

`due_at` хранится в UTC. Paused schedules не должны попадать в due projection.
При ошибке concurrent submit проверяются `learning_mutations`, unique
`(session_id, item_id)` и schedule row revision; ослаблять эти ограничения
нельзя.

