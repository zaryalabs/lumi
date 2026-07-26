# ADR 0026: Learning completion, versioned items и immutable sessions

Status: accepted

## Контекст

Первый learning-эпик должен связать завершение чтения с необязательной
самопроверкой, не превращая UI Reader в источник доменной истины. Повтор запроса,
reload страницы, новая редакция вопроса и открытие источника во время ответа не
должны дублировать completion/attempt или менять уже начатую сессию.

Контур обязан работать без AI provider. Поэтому закрытые ответы и explicit
self-check открытого ответа должны иметь детерминированную семантику, а будущие
FSRS, AI evaluation и voice остаются расширениями тех же contracts.

## Решение

1. `LearningSource` является immutable identity конкретной
   `DocumentRevision` и одного scope: material, content unit или source-backed
   anchor. `scope_key` дедуплицирует эту identity в пределах аккаунта.
2. `ReadingCompletion` записывается до построения offer. Пара
   `(user, source, completion_generation)` уникальна; mutation также защищена
   `Idempotency-Key`. В E1 используется первая generation. Момент первого показа
   и dismiss хранятся отдельно, поэтому автоматический offer показывается один
   раз и не влияет на reading progress.
3. `LearningItem` хранит lifecycle и ссылку на текущую immutable
   `LearningItemRevision`. Редактирование создаёт новую revision. Draft можно
   активировать, active item — архивировать; старые revisions не переписываются.
4. При создании `LearningSession` сервер выбирает только active items того же
   source и сохраняет ordered snapshot presentation вместе с точным
   `item_revision_id`. Reload читает эту же сессию, даже если item позже
   отредактирован.
5. `LearningAttempt` append-only и уникален для пары `(session, item)` в E1.
   Submit, session transition и sync change фиксируются в одной транзакции.
   Неотвеченные items при завершении сессии не получают attempt и оценку.
6. `single_choice`, `multiple_choice`, `true_false` и `cloze` проверяются в
   `lumi-core` без provider. `open_question` и flashcard требуют явной
   self-check оценки пользователя. Authoritative answer не входит в session
   presentation и появляется только в feedback после submit/reveal.
7. Learning objects принадлежат Personal SyncSpace. Item/settings используют
   object revisions; attempts и evidence events являются append-only. В E1
   сервер добавляет совместимые records в существующий `sync_changes`.
8. `Scheduler` задаётся platform-independent port в `lumi-core`. Schedule,
   FSRS state и алгоритмические версии вводятся отдельной migration эпика E2.

## API

Системная граница находится под `/api/v1`:

```text
POST  /materials/{material_id}/reading-completions
GET   /materials/{material_id}/learning-offer?source_id=...
PATCH /materials/{material_id}/learning-settings

GET|POST /learning/items
GET|PATCH /learning/items/{item_id}
POST /learning/items/{item_id}/activate
POST /learning/items/{item_id}/archive

POST /learning/sessions
GET  /learning/sessions/{session_id}
POST /learning/sessions/{session_id}/start
POST /learning/sessions/{session_id}/items/{item_id}/source-opened
POST /learning/sessions/{session_id}/items/{item_id}/attempts
POST /learning/sessions/{session_id}/complete
POST /learning/sessions/{session_id}/abandon
```

Все mutations требуют account session, CSRF и `Idempotency-Key`. Material,
revision, source, item и session разрешаются только внутри владельца.

## Последствия

- Completion и reading progress независимы; сбой learning не откатывает чтение.
- История попыток воспроизводима по точной revision вопроса.
- Сессия безопасно продолжается после reload и переживает редактирование items.
- Ручной deterministic flow полезен без provider и не ждёт будущую генерацию.
- Один item нельзя повторно ответить в той же immediate-recall session; повторные
  попытки и scheduling добавляются в E2.
- E1 хранит typed JSON payload рядом с relational ownership/lifecycle columns.
  Это сохраняет эволюцию DTO, но требует schema validation в application layer.

## Альтернативы

- `rejected`: вычислять completion из текущей страницы Reader. Это делает
  событие недолговечным и повторяет offer после reload.
- `rejected`: изменять item revision на месте. Это ломает объяснимость старых
  attempts.
- `rejected`: оценивать закрытые ответы через AI. Это добавляет стоимость,
  нестабильность и недетерминированность без продуктовой пользы.
- `rejected`: хранить session только в браузере. Это не поддерживает
  reload/resume и будущие native replicas.

## Совместимость

Forward-only migration `20260726210000_learning_core.sql` создаёт sources,
completions, settings, mutations, versioned items, hints/rubrics, sessions,
session snapshots, attempts и evidence events. Capability
`learning-core` публикуется только вместе с route group `learning`.

Unit/HTTP/PostgreSQL tests проверяют grading, idempotency, immutable snapshot,
owner isolation, atomic submit и отсутствие оценки для unanswered items.
Browser acceptance проверяет offer после durable progress, ручной запуск,
source jump и продолжение сессии по hash route.
