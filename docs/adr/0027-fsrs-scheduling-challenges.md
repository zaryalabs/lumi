# ADR 0027: FSRS scheduling, evidence и bounded Challenges

Status: accepted

## Контекст

Learning E1 сохраняет immutable session snapshots и append-only attempts, но не
решает, когда возвращать вопрос. E2 должен учитывать успешность вспоминания,
подсказки и открытие источника, не выдавая помощь за самостоятельный recall.
Pause, disable, manual-only и snooze не должны удалять историю или превращать
время вне расписания в штрафной backlog.

## Решение

1. `lumi-core::Scheduler` принимает предыдущий `LearningSchedule`, явный
   `again | hard | good | easy`, deterministic outcome, self-check, раскрытые
   hints, source-opened и UTC timestamp. Результат содержит полный новый
   schedule и консервативный suggested rating.
2. Первый adapter — локальный deterministic FSRS 4.5 с published default
   weights, desired retention `0.9` и идентификатором
   `fsrs-4.5-lumi-v1`. Формулы и параметры находятся в собственном небольшом
   adapter, поэтому runtime не получает optimizer/tensor dependency, а
   regression vectors воспроизводимы. Рассмотренные upstream Rust реализации:
   `rs-fsrs` (MIT, scheduler) и `fsrs-rs` (BSD-3-Clause, scheduler +
   optimizer). Переход на FSRS 5/6 выполняется как новая algorithm version,
   без переписывания attempts.
3. Mapping suggested rating:
   - incorrect, blank или `not_recalled` → `again`;
   - верный/self-checked ответ с hint, source-opened или `partial` → `hard`;
   - самостоятельный верный recall → `good`;
   - `easy` никогда не предлагается автоматически, но остаётся явным выбором.
4. Attempt, evidence events и schedule upsert коммитятся одной PostgreSQL
   transaction. Lock session и schedule row плюс idempotency mutation не дают
   двум concurrent submit обновить schedule дважды.
5. `LearningSchedule` хранит state, UTC `due_at`, stability, difficulty,
   repetitions, lapses, algorithm/version, opaque payload, pause marker и
   object revision. Immutable attempts остаются единственным replay source.
6. Account settings задают `scheduling_enabled`, daily limit `1..=100`
   (default `20`) и `manual_only`. Source override хранит enable/pause.
7. Pause исключает source из due/overdue и сохраняет предыдущее state в
   algorithm payload. Resume восстанавливает state и делает старое due
   actionable не раньше текущего момента; projection всё равно ограничена
   daily limit.
8. Snooze переносит только unanswered schedule items bounded session и
   завершает сессию как abandoned. Ни attempt, ни fake failure не создаются.
9. `GET /learning/challenges/today` возвращает bounded due groups, bounded
   unscheduled ready groups и отдельные unbounded counts. Paused sources не
   входят в due count.
10. Hints раскрываются строго по порядку отдельными idempotent commands.
    `hint_revealed` и `source_opened` читаются при submit и попадают в attempt и
    scheduler evidence.

## Последствия

- Scheduling полезен без AI provider и публикуется capability
  `learning-scheduling`.
- Один алгоритм не становится частью domain model; upgrade требует нового
  adapter/version и regression/replay gate.
- `manual_only` скрывает автоматическую `Сегодня`, но не schedule state и не
  ручную практику.
- Material pause можно отменить без удаления item, attempt или schedule.
- Текущий adapter округляет междневные интервалы минимум до одного дня;
  отдельный intraday learning-step adapter можно добавить совместимо.

## API

```text
GET   /api/v1/learning/challenges/today
GET   /api/v1/learning/schedules
GET   /api/v1/learning/settings
PATCH /api/v1/learning/settings
GET   /api/v1/learning/sources/{source_id}/settings
POST  /api/v1/learning/sources/{source_id}/pause
POST  /api/v1/learning/sources/{source_id}/resume
POST  /api/v1/learning/sessions/{session_id}/snooze
POST  /api/v1/learning/sessions/{session_id}/items/{item_id}/hints/{position}/reveal
```

Mutations используют существующие account session, CSRF, ownership и
`Idempotency-Key` contracts.

## Совместимость

Forward-only migration `20260726220000_learning_scheduling.sql` создаёт пустые
settings/schedules/source overrides/snoozes. Старые items не получают schedule
до первой реальной попытки и появляются в `Закрепить сейчас`, но не как
overdue. Старый client продолжает использовать `learning-core`.

