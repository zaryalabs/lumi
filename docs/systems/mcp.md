# MCP-интерфейс для внешних агентов

Status: draft

## Контекст

MCP в Lumi — внешний программный интерфейс, через который подключенный агент
может работать с пользовательским пространством приложения. Он не
ограничивается AI task queue и не является способом встроить agent UI или
agent conversation внутрь Lumi.

Целевой принцип:

> Если обычный пользователь может выполнить продуктовую операцию через Lumi,
> внешний агент должен иметь возможность выполнить эквивалентную доменную
> команду через MCP, кроме административных, credential и security-sensitive
> операций.

Один MCP server поддерживает два связанных сценария:

- агент действует как пользователь: импортирует и читает материалы, ищет,
  создает annotations, работает с KB/learning/social и экспортом;
- агент действует как AI worker: получает queued tasks, выполняет их и
  возвращает typed results.

## Основные принципы

- **User parity.** Целевой MCP surface покрывает пользовательские application
  commands, а не только AI tools.
- **Same application layer.** Web UI и MCP вызывают одни domain/application
  services.
- **External boundary.** MCP используется внешним агентом и не является частью
  глобального чата Lumi.
- **Account scope.** Подключение действует только от имени одного пользователя.
- **No secrets.** Агент не получает seed phrase, BYOK credentials, provider
  secrets или server tokens.
- **Typed tools.** Tools имеют понятные input/output contracts; универсального
  `execute_action(name, json)` нет.
- **Source-backed operations.** Чтение, annotations и AI context сохраняют
  material/revision/anchor provenance.
- **Async work.** Import, export и AI workflows возвращают job/task ids, а не
  блокируют MCP request до завершения.
- **Progressive coverage.** Tool surface расширяется вместе с реализованными
  подсистемами; capabilities сообщают фактически доступные операции.

## Граница application layer

MCP не создает параллельную бизнес-логику:

```text
Web UI ───────┐
              ├── Application Commands -> Domain Services
MCP tools ────┘
```

Например, web action и MCP tool создания заметки используют одну команду
`CreateNote`. Проверки владельца, anchors, idempotency, validation, jobs и sync
changes остаются общими.

MCP tools описывают доменные возможности, а не элементы UI. Например,
`create_annotation` корректнее, чем
`click_reader_yellow_highlight_button`.

## Авторизация и подключение

Первая версия использует простой account-scoped full-access token:

1. Пользователь открывает настройки MCP.
2. Создает отдельный revocable token.
3. Передает endpoint/token внешнему агенту.
4. Агент действует с обычными product permissions этого аккаунта.
5. Пользователь может отозвать подключение.

Token:

- не является browser session cookie;
- хранится через secret storage;
- не входит в ordinary plaintext sync;
- не раскрывает seed phrase или BYOK;
- не дает admin/system permissions;
- привязан к одному account/user;
- может быть отозван независимо от web sessions.

В первом срезе не требуется отдельная сложная capability/grant система или
детальный MCP audit subsystem. Read-only tokens и per-tool scopes можно
добавить позже при подтвержденной потребности.

## Capability discovery

Agent сначала может вызвать:

```text
get_lumi_capabilities()
```

Результат сообщает:

- доступные import source types/formats;
- реализованные material/reader operations;
- наличие global/material search;
- доступность annotations, KB, learning и social;
- поддерживаемые AI task kinds и artifact kinds;
- export formats;
- ограничения размера, pagination и enabled server features.

Не реализованные подсистемы не должны имитироваться пустыми tools. Server
возвращает фактические capabilities текущей Lumi instance.

## Библиотека и импорт

Целевые операции:

```text
list_materials(filters, cursor)
get_material(material_id)
import_file(file_ref, options, idempotency_key)
import_url(url, options, idempotency_key)
import_text(text, metadata, idempotency_key)
get_import_status(job_id)
archive_material(material_id)
restore_material(material_id)
prepare_delete_material(material_id)
confirm_delete_material(confirmation_token)
download_material(material_id)
```

Import tools используют тот же durable import/Job pipeline, что web UI.
Большая загрузка может использовать отдельный upload/blob flow, после чего
`import_file` получает безопасную ссылку на загруженный объект.

## Чтение материалов

Целевые операции:

```text
get_material_toc(material_id)
get_chapter(material_id, chapter_ref)
read_material_chunks(material_id, scope, cursor, limit)
search_in_material(material_id, query, options)
get_source_context(source_ref, options)
set_reading_progress(material_id, location, intent)
```

Большая книга не возвращается одним MCP response. Agent читает главы или
source-backed chunks с pagination, anchors и metadata.

Чтение агентом не меняет человеческий `ReadingProgress` автоматически.
`set_reading_progress` является отдельной явной командой, если агенту
действительно нужно выполнить пользовательское действие изменения позиции.

## Аннотации

Целевые операции:

```text
list_annotations(scope, filters, cursor)
create_highlight(target, style, idempotency_key)
create_note(target, body, idempotency_key)
create_margin_note(target, body, idempotency_key)
create_bookmark(target, idempotency_key)
update_annotation(annotation_id, changes, expected_revision)
delete_annotation(annotation_id, expected_revision)
```

`scope` поддерживает один material или весь доступный personal account.
Межматериальная группировка, learning state и сохраненные artifacts доступны
через Desk operations:

```text
list_desk_materials(filters, cursor)
get_material_desk(material_id)
list_desk_items(scope, filters, cursor)
get_desk_item(object_type, object_id)
resolve_desk_link(text, context)
```

Target использует общую anchor model. Агент сохраняет quote/source refs и не
создает собственные DOM/offset-only привязки.

Private note и будущий shared comment являются разными командами и сущностями.

## Поиск

Целевые операции:

```text
search(query, scope, filters, cursor)
search_material(material_id, query, filters, cursor)
search_notes(query, filters, cursor)
get_search_result_context(result_ref, options)
```

Search соблюдает account/shared permissions и возвращает source refs/anchors.
Agent может продолжить чтение, создать annotation или использовать результат
как context для отдельного пользовательского действия.

## KB, learning и social

По мере реализации подсистем MCP покрывает их пользовательские команды:

```text
list_kb_notes
create_kb_note
update_kb_note
delete_kb_note

list_learning_items
create_flashcard_task
submit_learning_answer

list_community_spaces
get_community_space
share_material_to_space
list_shared_comments
create_shared_comment
update_shared_comment
delete_shared_comment
list_space_chat_messages
create_space_chat_message
```

Social tools всегда используют обычные ACL и membership. Команда
`share_material_to_space` публикует shared material identity/claim, но не
передает source blob или private annotations. Создание shared comment является
явным действием и не заменяет private note.

## AI task queue

Queue-oriented tools:

```text
list_ai_tasks(filters)
claim_ai_task(task_id)
get_ai_task_context(task_id)
update_ai_task_progress(task_id, progress)
complete_ai_task(task_id, result)
fail_ai_task(task_id, reason)
release_ai_task(task_id)
```

Agent получает kind, instruction, expected result, разрешенные context refs и
target anchors. Он возвращает typed result: `SummaryArtifact`, flashcard set,
validated `.lum` input/package или другой ожидаемый output.

Agent также может создавать AI tasks через обычные пользовательские application
commands, например `create_summary_task` или `create_abridgement_task`.

Полный queue contract описан в
[`ai-task-queue.md`](ai-task-queue.md).

## Саммари и производные материалы

Целевые операции:

```text
get_summary(source_scope)
create_summary_task(source_scope, form, execution)
update_summary(summary_id, content, expected_revision)
delete_summary(summary_id)
create_abridgement_task(material_id, options, execution)
```

`execution` может оставить task в очереди или попросить внутреннее
`Выполнить сейчас`. Внешний agent затем может claim task через queue tools.

Сокращенная версия публикуется как отдельный derived `.lum` material по
контракту [`ai-summaries.md`](ai-summaries.md).

## Экспорт

Целевые операции:

```text
create_export(scope, format, options, idempotency_key)
get_export_status(job_id)
download_export(job_id)
```

Export использует общий durable Job engine и обычные permission/source rules.

## Граница глобального чата

MCP не вызывает внутренний chat runtime Lumi:

- agent не отправляет сообщения в глобальный чат от имени пользователя;
- chat turns не создаются через MCP;
- failed chat request не превращается в MCP/queue task автоматически;
- интерактивный диалог внешнего агента остается в интерфейсе агента.

Chat history также не входит в первый MCP scope. Если позже появится
пользовательский сценарий чтения или экспорта conversations, он должен быть
спроектирован отдельно, без циклического agent-to-chat invocation.

## Исключенные операции

MCP не открывает:

- seed phrase, auth verifier и recovery secrets;
- BYOK/provider API keys;
- Telegram bot token;
- admin bootstrap и назначение администратора;
- server/deployment settings;
- billing/subscription administration;
- secret storage;
- raw database/filesystem/blob internals;
- migrations и worker control plane;
- управление security credentials;
- удаление аккаунта;
- внутренний global chat runtime.

Управление device sessions и массовый отзыв sessions также не входят в первый
MCP scope. Эти операции можно пересмотреть отдельно, но они не относятся к
обычному product content surface.

## Destructive и publication actions

Обратимые действия вроде archive выполняются одной командой. Необратимые или
публикующие действия используют двухшаговый contract:

```text
prepare_action(exact_target)
  -> consequences + confirmation_token

confirm_action(confirmation_token)
  -> mutation
```

Такой flow нужен как минимум для:

- окончательного удаления material;
- массового удаления объектов;
- публикации private content в shared/public surface;
- других необратимых операций с большим scope.

Confirmation token привязан к exact account, action, targets, ожидаемым
revisions и короткому сроку действия. Это не отдельная approval-платформа, а
защита от случайного или устаревшего tool call.

## Contract rules

- Cursor pagination для больших списков и material chunks.
- Idempotency keys для imports, creates и повторяемых mutations.
- `expected_revision` для конфликтных updates/deletes.
- Exact ids вместо неограниченных строковых targets для destructive actions.
- Typed `application/problem+json`-like errors в адаптированной MCP response
  форме.
- Content/size limits совпадают с обычным API.
- Tool result остается компактным; большие blobs передаются через bounded
  upload/download flow.
- Tool names остаются стабильными, но отдельный schema registry/versioning
  subsystem в первом срезе не нужен.

## Первый вертикальный срез

1. Создание и отзыв account-scoped MCP token.
2. `get_lumi_capabilities`.
3. List/get/import/read materials.
4. Material/global search, если search subsystem доступна.
5. CRUD highlights, notes и bookmarks.
6. Создание AI tasks.
7. Полная работа с AI task queue.
8. Получение готовых artifacts/derived materials.
9. Archive/restore material.
10. Двухшаговый permanent delete.

KB, learning, social и export tools добавляются вместе с соответствующими
реализованными пользовательскими surface.

## Нефункциональные требования

- **Parity.** MCP и Web используют одинаковые application commands.
- **Isolation.** Token ограничен одним account и не повышает privileges.
- **Revocation.** Пользователь может независимо отключить agent connection.
- **Idempotency.** Retried tool calls не создают дубликаты.
- **Concurrency.** Expected revisions и atomic claims защищают mutations/tasks.
- **Bounded context.** Материалы читаются chunks/pages, а не целиком без limits.
- **Portability.** MCP не становится единственным путем к пользовательским
  данным.
- **Replaceability.** External agent не зависит от конкретного AI provider
  Lumi.

## Альтернативы

- `accepted`: MCP стремится к parity со всеми product user operations.
- `accepted`: AI task worker является одной из семей tools общего MCP surface.
- `accepted`: простой revocable full-account token в первом срезе.
- `accepted`: двухшаговый contract для destructive/publication operations.
- `accepted`: capabilities отражают только фактически реализованные функции.
- `rejected`: ограничить MCP только AI task queue.
- `rejected`: отдельная MCP-specific business logic.
- `rejected`: открыть agent доступ к admin, credentials, chat runtime или
  account deletion.
- `rejected`: один универсальный untyped `execute_action`.

## Открытые вопросы

- Какой transport/deployment profile использовать для local и hosted
  подключений?
- Нужен ли read-only token после первого full-access среза?
- Как передавать большие upload/download blobs без раздувания MCP messages?
- Какие publication actions кроме permanent delete требуют двухшагового flow?
- Нужен ли отдельный MCP connection list с last-used metadata?
