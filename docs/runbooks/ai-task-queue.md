# AI-задачи и сохранённые саммари

Status: `accepted`

Этот runbook описывает production path эпиков `0.2.0/E2–E4`: durable очередь,
внутренний OpenRouter worker, сохранённые саммари главы/материала, защиту
ручных правок и выпуск производного `.lum`.

## Пользовательский flow

1. Настроить и проверить OpenRouter BYOK через глобальный AI-чат.
2. В Reader выбрать «Саммари главы» в конце структурного раздела либо
   «Саммари материала» в меню материала. Для PDF доступно саммари материала и
   текущей страницы как chapter scope.
3. Выбрать форму `Краткое` или `Структурный конспект` и создать саммари.
4. Lumi сначала сохраняет `AiTask`, затем `Выполнить сейчас` помечает ту же
   задачу для внутреннего worker. Диалог показывает durable server status.
5. Готовое саммари содержит citations. Переход по источнику открывает
   соответствующий immutable material/revision scope в Reader.
6. Страница «AI-задачи» показывает server-backed очередь, фильтр состояний,
   выбор задач, bulk execute, cancel и retry.

Ручное редактирование создаёт новую immutable user-authored revision artifact.
Следующая генерация становится `candidate` и не заменяет её автоматически.
Только явное «Принять» переводит candidate в active; «Отклонить» сохраняет
ручную active-версию.

## Выполнение и восстановление

Внутренний worker запускается вместе с server process и опрашивает только
задачи с явным `internal_execution_requested`. Claim использует общий fenced
Job Runtime и lease 15 минут. Heartbeat продлевает lease и фиксирует стадии:

```text
context
  -> section_summaries
  -> synthesis
  -> coverage_check
  -> artifact_validation
```

Для bounded небольшого context выполняется один structured provider call. Для
контекста больше 8 fragments или 64 KiB worker сначала строит промежуточные
саммари порциями около 32 KiB, затем синтезирует итог по исходному immutable
context pack. Финальный payload обязан пройти frozen
`summary-artifact.v1` validation и проверку citation IDs до транзакционной
публикации.

Для material-scoped abridgement worker использует
`abridgement-artifact.v1`, а после structured result последовательно отмечает
`package_assembly`, `package_validation`, `import` и `publication`. ZIP не
является входом AI executor: сервер собирает его из проверенных chapter units и
повторно использует обычный `.lum` importer. Подробная диагностика и recovery
описаны в [derived-materials.md](derived-materials.md).

При старте worker вызывает recovery общего Job Runtime. Истёкший claim
освобождается для следующей попытки, старый fence больше не может опубликовать
результат. Максимум попыток хранится в job row. Retryable provider failure
возвращает job в очередь; terminal source/schema/auth failure фиксируется как
ошибка и требует явного retry после устранения причины.

Cancel queued task завершает её сразу. Для running task выставляется durable
`cancellation_requested`; worker проверяет его на heartbeat boundaries и не
публикует artifact после отмены.

## HTTP surface

Все маршруты account-scoped и находятся под `/api/v1`:

- `GET|POST /ai/tasks`;
- `GET /ai/tasks/{task_id}`;
- `POST /ai/tasks/{task_id}/execute|cancel|retry`;
- `POST /ai/tasks/bulk-execute`, не более 50 task IDs;
- `GET /materials/{material_id}/summaries`;
- `POST /materials/{material_id}/summary-tasks`;
- `POST /materials/{material_id}/abridgement-tasks`;
- `PATCH|DELETE /ai/summaries/{summary_id}`;
- `GET /ai/artifacts/{artifact_id}`;
- `POST /ai/artifacts/{artifact_id}/accept|reject`.

Mutations используют CSRF, request idempotency key и expected object revision.
Owner scope применяется внутри SQL; foreign task/artifact выглядит как
`not found`.

## Диагностика

- `missing_credential` — настроить OpenRouter BYOK и повторить задачу;
- `provider_authentication` — ключ отклонён provider;
- `rate_limited`, `provider_timeout`, `provider_unavailable` — retryable
  upstream failure;
- `source_revision_unavailable`, `source_scope_unavailable` — исходная
  immutable revision или scope недоступны;
- `source_text_unavailable` — нет пригодного text layer;
- `invalid_structured_result` — provider вернул payload вне frozen schema.

В logs допустимы task/run IDs, redacted error code и stage. Нельзя логировать
credential, provider body, полный context pack или текст саммари.

## Проверка

```sh
make l
make pg-t
make web-e2e
make c
```

PostgreSQL suite проверяет recovery/fencing, idempotent completion и политику
manual edit/candidate. Playwright использует локальный OpenRouter-compatible
mock и проходит сквозной flow создания, редактирования, перегенерации и Queue.
