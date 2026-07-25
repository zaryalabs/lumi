# Очередь ИИ-задач

Status: accepted

## Контекст

Очередь ИИ-задач нужна для фоновых, отложенных и пакетных операций: создать
саммари главы, подготовить карточки, обработать заметки, расшифровать аудио или
собрать сокращенный `.lum`-материал.

Каждая такая операция сначала становится durable `AiTask`, после чего
пользователь выбирает способ исполнения:

- оставить задачу в очереди для внешнего агента или будущего запуска;
- сразу запустить через внутренний provider с настроенным BYOK-ключом.

Внутренний provider и внешний MCP-агент работают с одним task/result contract.
Способ исполнения не должен менять форму итогового artifact или derived
material.

## Граница с глобальным чатом

Глобальный чат не входит в `AiTask` queue.

- Chat message использует прямой low-latency provider call.
- Ответы, объяснения и быстрые саммари внутри чата не создают `AiTask`.
- Если provider/key недоступен или ответ чата не помог, chat flow
  останавливается без автоматического fallback в очередь.
- Пользователь может отдельно запустить соответствующее фоновое действие,
  например «Создать саммари главы», и тем самым создать обычный `AiTask`.

Это сохраняет понятную продуктовую границу: чат является разговором, а очередь
— местом управления отложенной работой и результатами.

## Создание задачи

Любое фоновое AI action предлагает два пути:

- `Добавить в очередь`;
- `Выполнить сейчас`.

Оба пути сначала создают одну и ту же durable task. `Выполнить сейчас`
дополнительно пытается сразу назначить ее внутреннему server worker.

```text
AI action
  -> create AiTask
  -> enqueue
     or
  -> claim by internal worker
```

Если BYOK credential не настроен, `Выполнить сейчас` недоступно и UI предлагает
добавить ключ. `Добавить в очередь` остается доступным: такую задачу позже
может выполнить пользователь или внешний агент.

Для `transcribe_audio`/`transcribe_voice_note` встроенный worker использует не
OpenRouter, а отдельный account-scoped OpenAI API credential и OpenAI Audio
Transcriptions API с model `whisper-1`, как зафиксировано в
[ADR 0025](../adr/0025-openai-whisper-transcription.md). Отсутствие OpenAI key
не блокирует постановку задачи в очередь, но блокирует ее немедленное
встроенное исполнение.

По умолчанию наличие ключа не запускает queued tasks автоматически.
Автоматические правила исполнения могут быть добавлены позже отдельной
настройкой.

Повторное нажатие не должно создавать одинаковые активные задачи. Если
эквивалентная task уже `queued` или `running`, Lumi открывает существующую
задачу или ее progress.

## Панель очереди

Для текущего web scope очередь является отдельной AI surface, а не частью
глобального чата. Основное представление на desktop-width web — таблица.

Рамочные колонки:

- selection checkbox;
- тип задачи;
- material/chapter/target;
- пользовательский status;
- время создания;
- текущий исполнитель;
- progress;
- доступные действия.

Основные операции:

- фильтровать задачи по status, type и material;
- открыть source target;
- выделить несколько queued tasks;
- выполнить выбранные задачи внутренним provider;
- отменить queued/running task;
- повторить failed task;
- открыть готовый artifact или derived material;
- убрать завершенную task из рабочего списка без удаления результата.

Bulk execution назначает выбранные задачи server worker. Browser не должен
последовательно вызывать provider самостоятельно.

## Статусы

Пользовательские состояния:

- `В очереди`;
- `Выполняется`;
- `Нужны данные`;
- `Готово`;
- `Ошибка`;
- `Отменено`.

Domain lifecycle может быть подробнее:

- `queued`;
- `claimed`;
- `running`;
- `needs_input`;
- `succeeded`;
- `failed`;
- `cancelled`;
- `expired`.

`claimed`, lease, retry counter и worker heartbeat являются техническими
деталями и не обязаны отображаться в основной таблице.

## Исполнители

Task может быть:

- без исполнителя;
- назначена внутреннему provider worker;
- claimed внешним MCP-агентом.

Queued task не закрепляется за исполнителем навсегда. Пользователь может позже
нажать `Выполнить сейчас`, если внешний агент еще не забрал ее. Внутренний
worker и внешний агент используют atomic claim, поэтому одна task не
исполняется двумя сторонами одновременно.

Минимальный lease нужен для восстановления после падения исполнителя. После
истечения lease task возвращается в доступное состояние либо retry policy
переводит ее в `failed`.

## MCP

Общий пользовательский MCP surface описан в [`mcp.md`](mcp.md). Queue tools
являются его worker-oriented частью.

Минимальный queue-oriented MCP surface:

```text
list_ai_tasks(filters)
claim_ai_task(task_id)
get_ai_task_context(task_id)
update_ai_task_progress(task_id, progress)
complete_ai_task(task_id, result)
fail_ai_task(task_id, reason)
release_ai_task(task_id)
```

Agent получает:

- kind и instruction;
- expected result kind/schema;
- разрешенные source/context refs;
- target material/chapter/anchor;
- параметры генерации;
- команды завершения.

Agent возвращает ожидаемый typed result, а не только произвольный текст.
Например, summary task создает `SummaryArtifact`, а abridgement task —
validated `.lum` package/derived material.

Batch claim можно добавить позже. Первая версия может использовать обычный
list + atomic claim отдельных задач.

## Результат и попытка исполнения

Task, execution attempt и результат являются разными сущностями:

```text
AiTask
  -> AiRun[]
  -> AiArtifact | Derived Material | other typed result
```

Это позволяет:

- retry task после ошибки;
- сменить исполнителя;
- сохранить diagnostics предыдущей попытки;
- принять новый candidate без тихого удаления пользовательских изменений.

Успешная task остается доступной из истории, но основной пользовательский
объект — созданный artifact или material.

## Сложные workflow

Многошаговая операция показывается одной parent task. Например, сокращение
книги может внутри создавать processing units по главам, но не заполняет
пользовательскую таблицу десятками строк.

```text
Сократить книгу
  -> анализ структуры
  -> обработка глав
  -> synthesis
  -> сборка .lum
  -> validation/import
```

Пользователь видит общий progress и при необходимости раскрывает этапы.
Внутренние units принадлежат Job/workflow engine и не являются независимыми
пользовательскими `AiTask`.

## Mobile note

Mobile client не входит в текущий scope queue UI. При будущем проектировании
нужно учесть:

- таблица должна превратиться в компактное list/card presentation;
- bulk selection требует отдельного touch-friendly режима;
- background execution ограничивается lifecycle и network policy мобильной ОС;
- direct client-local provider execution может прерываться при уходе
  приложения в background;
- server/MCP execution и синхронизация результатов должны оставаться
  доступными независимо от состояния mobile client.

Эти нюансы не меняют общий `AiTask` contract и не требуют mobile реализации в
текущем срезе.

## Нефункциональные требования

- **Durability.** Task переживает reload и restart worker.
- **Single execution.** Atomic claim не допускает одновременное исполнение.
- **Idempotency.** Повтор action/complete не создает дубликаты.
- **Cancellation.** Worker регулярно проверяет cancellation.
- **Typed result.** Результат валидируется до публикации.
- **Provider parity.** Internal provider и MCP agent используют одинаковые
  task/result contracts.
- **No browser secrets.** Web queue execution использует server-side BYOK
  credential.

## Интеграции

- **AI core.** Определяет task, run, provider и result contracts.
- **Глобальный чат.** Не использует очередь и не создает `AiTask`.
- **Reader.** Создает summary/learning tasks из chapter/material actions.
- **Саммари.** Chapter/material summary и abridged `.lum` выполняются через
  queue.
- **Job engine.** Дает leases, retry, progress, cancellation и recovery.
- **MCP.** Позволяет внешнему агенту работать с пользовательским пространством
  и claim/complete tasks по контракту [`mcp.md`](mcp.md).
- **Artifacts/KB/Learning.** Принимают validated typed results.

## Альтернативы

- `accepted`: все фоновые AI operations сначала создают durable `AiTask`.
- `accepted`: `Выполнить сейчас` использует ту же очередь и сразу назначает
  task внутреннему worker.
- `accepted`: отдельная web-таблица с bulk selection/execution.
- `accepted`: внешний агент исполняет те же tasks через MCP.
- `accepted`: глобальный чат не входит в task queue.
- `rejected`: прямой provider call для фоновых действий в обход `AiTask`.
- `rejected`: автоматически превращать failed/unavailable chat request в
  queued task.
- `rejected`: показывать внутренние workflow units как отдельные
  пользовательские задачи.

## Принятый профиль `0.2.0`

- Terminal task metadata сохраняется как пользовательская история и может быть
  скрыта из рабочего списка. Run transport diagnostics по умолчанию хранятся
  30 дней; artifact/derived material и их provenance живут по своей lifecycle
  policy. Автоматического удаления пользовательского результата вместе с task
  нет.
- Priority определяется task kind и admission policy. Ручное изменение порядка
  и drag-and-drop не входят в первый срез.
- Bulk execute ограничено 50 tasks и показывает доступный provider/model,
  количество задач и известные hard limits. Точный token/cost estimate не
  является gate, пока provider не дает надежную оценку.
- Expired claim requeue-ится только для retryable failure при оставшемся retry
  budget и отсутствии cancellation. Exhausted/non-retryable attempt переводит
  task в `failed`.
- Основная таблица имеет filters, включая terminal statuses, и toggle
  «Показывать завершенные»; отдельная history surface не нужна в `0.2.0`.

Точная граница task/run/claim закреплена в
[ADR 0019](../adr/0019-ai-task-run-artifact-schema.md), общий execution runtime
— в [ADR 0020](../adr/0020-common-job-runtime.md).
