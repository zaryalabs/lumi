# ИИ-функционал

Status: accepted

## Контекст

ИИ в Lumi нужен для задач, которые помогают читать, понимать и превращать
материалы в знания:

- объяснить выделенный фрагмент;
- ответить на вопрос по тексту;
- сделать summary/outline;
- создать карточки, вопросы и тесты;
- выделить сущности, понятия и связи;
- помочь в explain-back упражнении;
- собрать черновики KB notes.

ИИ-слой должен быть заменяемым. Пользователь может:

- добавить свой API key;
- в будущем использовать встроенную подписку/серверный provider; подписка не
  входит в текущий scope реализации;
- отключить ИИ;
- подключить внешнего агента, который будет обрабатывать очередь задач.

Первичный provider target: OpenRouter через OpenAI-compatible API. Архитектура
не должна быть жестко привязана к OpenRouter, потому что будущие providers,
локальные модели и external agents должны использовать тот же task/artifact
contract.

## Пользовательские сценарии

- Пользователь выделяет абзац и спрашивает "объясни проще".
- Пользователь открывает глобальный сворачиваемый чат с любого экрана, создает
  отдельные чаты и ведет обычный многошаговый диалог с моделью.
- Пользователь передает в чат выделение, главу, материал или другой явно
  выбранный контекст и задает по нему вопрос.
- Пользователь запускает "сделай карточки по главе"; задача попадает в очередь,
  а результат появляется позже.
- Пользователь не добавил API key. External agent читает очередь и записывает
  summaries/questions обратно как artifacts.
- Пользователь работает в web-клиенте. На первом этапе BYOK key хранится в
  защищенном server-side secret storage. В будущем native-клиенты смогут
  выбрать client-local хранение и выполнение без передачи ключа серверу.
- Пользователь запускает explain-back внутри Lumi. Это работает только при
  direct AI provider с настроенным ключом.
- Пользователь просит external agent провести explain-back. Agent открывает
  собственный UI и возвращает final artifact/attempt summary to Lumi.
- Пользователь выбирает, какие AI artifacts принять в KB/learning.

## Функциональные требования

### AI task queue

Полный рамочный контракт очереди, immediate execution, bulk actions и MCP
описан в [`ai-task-queue.md`](ai-task-queue.md).

Все неинтерактивные AI сценарии оформляются как durable tasks:

- summarize material/chapter/selection;
- explain selection as saved artifact;
- generate questions/cards;
- extract entities/concepts/links;
- transcribe voice note;
- clean up/import note;
- propose KB links;
- summarize shared discussion where allowed.

Task lifecycle:

- `queued`;
- `claimed`;
- `running`;
- `needs_input`;
- `succeeded`;
- `failed`;
- `cancelled`;
- `expired`.

Task has context policy: какие source chunks, notes, shared data и personal
data можно включать.

`AiTask` является domain entity. Фактическое исполнение идет через общий
`Job` engine: тот же lifecycle, leases, retry, progress, cancellation and
diagnostics используются для import, indexing, transcription, export/delete and
anchor repair. Это не должна быть отдельная несовместимая очередь только для
AI.

Пользователь может оставить task в очереди либо выбрать `Выполнить сейчас`,
чтобы тот же `AiTask` сразу попытался claim внутренний server worker с
настроенным BYOK. Внешний агент получает queued tasks через MCP.

### Interactive chat

Глобальный чат — полноценная пользовательская поверхность ИИ, доступная с
любого экрана и сворачиваемая на всех клиентах. Рамочный продуктовый контракт
описан отдельно в [`ai-chat.md`](ai-chat.md).

Chat отличается от background tasks и не создает `AiTask`:

- user expects streaming/low-latency response;
- пользователь может создавать, переименовывать, переключать и удалять чаты;
- поддерживается полный цикл ответа модели: отправка, streaming, остановка,
  повтор ответа, продолжение диалога и обработка ошибки;
- context may be transferred explicitly from the current selection, chapter,
  material, library search, KB or attachments;
- conversation history is stored as `AiConversation`;
- состояние открытого или свернутого чата и активный разговор сохраняются при
  переходе между экранами;
- user can choose to save an answer as KB note or artifact.

Chat inside Lumi requires available direct provider. MCP integration относится
к внешней работе агента с Lumi и не является способом встроить agent UI или
agent conversation внутрь чата Lumi. Если provider недоступен или chat response
не решил задачу пользователя, Lumi не превращает запрос в queued task
автоматически. Пользователь может отдельно запустить соответствующее фоновое
действие.

### Selection actions

Первый selection slice:

- explain selected text;
- summarize section;
- ask about selection;

Последующие actions поверх learning/search/KB:

- turn highlight into note;
- create questions/cards;
- find related notes/materials.

Reader creates context with:

- anchor;
- quote;
- surrounding blocks;
- material metadata;
- user instruction;
- selected output type.

В первом срезе объяснение или краткое содержание выделенного фрагмента
передается в глобальный чат. Reader открывает чат, прикрепляет selection context
и либо подставляет выбранную быструю команду, либо оставляет пользователю поле
для собственного вопроса. Отдельный сохраненный artifact для краткого саммари
выделения на этом этапе не обязателен.

Selection, chapter и material context строятся через общий
`SourceContextResolver`. Он разрешает только явно выбранный revision-bound
scope, возвращает bounded source refs/citations и не требует search index.
Действия `create questions/cards` и `find related notes/materials` включаются
отдельными capabilities после готовности learning и indexed retrieval.

### Summary forms

Рамочный продуктовый контракт саммари описан отдельно в
[`ai-summaries.md`](ai-summaries.md).

Саммари не является одним универсальным output type:

- `brief` — короткое содержание в нескольких тезисах;
- `outline` — структурированный конспект;
- `chapter_summary` — сохраненное саммари главы или раздела;
- `material_summary` — сохраненное саммари всего материала;
- `abridged_material` — отдельный сокращенный производный `.lum`-материал,
  который можно открыть и читать в обычном reader.

Саммари выделенного фрагмента в первом срезе остается ответом в чате. Саммари
главы или материала создается отдельным reader/material action и сохраняется
как `SummaryArtifact`. Для каждого source scope существует одно активное
саммари без параллельных именованных вариантов. `abridged_material` входит в
scope как отдельный durable workflow: он собирает `.lum` package и публикует
отдельный производный `Material` со ссылками на исходный `Material`,
`DocumentRevision` и использованные source anchors.

### Retrieval context

ИИ не должен получать entire library by default. Существуют два совместимых
пути получения контекста:

1. `SourceContextResolver` для selection/chapter/material scope с
   детерминированным обходом source;
2. `search.retrieve` для открытого material/library/record query после
   появления serious search.

```text
AiRequest
  -> scope/context policy
  -> SourceContextResolver | search.retrieve(...)
  -> context pack
  -> provider/agent
  -> artifact/conversation response
```

Оба пути возвращают совместимые source refs/citations. Context pack stores
citations и hashes, чтобы results могли ссылаться на sources. Только indexed
retrieval объявляет capability `ai-retrieval`; explicit context доступен через
отдельную capability `ai-explicit-context`.

### Provider model

Provider interface:

- OpenAI-compatible chat/completions for OpenRouter first.
- Structured output where possible for questions/cards/entities.
- Streaming for chat and explain-back.
- Batch/background calls for tasks.
- Provider capability metadata: max context, supports JSON schema, supports
  audio, supports embeddings, supports vision if ever needed.

Secrets:

- В первом web-срезе API keys хранятся в secure server-side secret storage.
- Будущий native mode может хранить ключ только на клиенте и выполнять запросы
  локально.
- Keys are not synced as plaintext.
- External agent credentials stay outside ordinary sync.

### External agent integration

Primary design: внешний агент подключается к Lumi через MCP interface.
MCP не встраивает интерфейс или чат агента в Lumi. Для AI-задач агент использует
ту же durable queue, что direct providers; optional CLI worker остается
fallback для простой автоматизации.

Полный account-scoped MCP contract и покрытие пользовательских application
commands описаны в [`mcp.md`](mcp.md). AI queue tools являются одной частью
этого внешнего интерфейса.

Для первого AI-среза agent capabilities включают:

- list tasks;
- claim task;
- read allowed context;
- write artifact/result;
- mark failed with reason;
- attach files if needed.

Целевое направление шире AI queue: MCP должен покрывать product operations,
которые доступны пользователю через приложение — работу с библиотекой,
материалами, reader state, annotations, search, KB, learning и social.
Агент действует от имени подключившего его пользователя и проходит те же
проверки доступа и доменные команды, а MCP tools остаются внешним интерфейсом к
существующим application services, а не параллельной бизнес-логикой.

На рамочном этапе не вводятся отдельные сложные подсистемы аудита или
версионирования MCP schemas. Достаточно account-scoped подключения, обычной
авторизации, понятных ошибок и безопасной обработки повторных команд. Более
строгие механизмы добавляются только при подтвержденной потребности.

MCP advantages:

- structured tool protocol;
- natural fit for Codex/agents;
- supports reading task metadata and writing outputs.

CLI fallback:

```text
lumi-ai-worker claim --task <id>
lumi-ai-worker complete --task <id> --result result.json
```

CLI is useful for simple automation and local scripts, but MCP is better for
interactive agents.

Platform scope:

- Desktop and web/server can support external agents.
- Mobile does not need agent integration initially.
- If mobile has no direct provider key, it can still see completed artifacts
  synced from other clients.

### Explain-back

Explain-back inside Lumi requires direct provider:

- streaming or quick turn-by-turn responses;
- conversation state;
- rubric/context;
- iterative correction.

Without direct provider:

- Lumi can create an external-agent task "run explain-back";
- agent conducts conversation in its own UI;
- agent returns final summary, score, missing concepts and optional KB note.

This preserves the learning value while keeping the external agent interaction
outside Lumi.

### Artifacts

AI output should become typed artifacts, not opaque chat text only:

- `SummaryArtifact`;
- `QuestionSetArtifact`;
- `FlashcardSetArtifact`;
- `EntityGraphArtifact`;
- `KbNoteDraft`;
- `TranscriptArtifact`;
- `ExplanationArtifact`;
- `LinkSuggestionArtifact`.

Artifacts can be:

- draft;
- accepted;
- rejected;
- superseded.

Only accepted artifacts should affect KB graph/search strongly by default.

## Нефункциональные требования

- **User control.** User decides provider/key and can disable AI.
- **Replaceability.** Providers and agents implement contracts, not UI-specific
  hacks.
- **Privacy.** Context inclusion is explicit and visible to the user.
- **Durability.** Background tasks survive reload/offline/retry.
- **Citation.** Source-backed answers should cite chunks/anchors where possible.
- **Cost control.** Tasks need estimates/limits and cancellation.
- **Validation.** Structured AI outputs are schema-validated before becoming
  learning/KB objects.
- **No silent publication.** AI artifacts are private until accepted/shared.

## Модель данных

```text
AiTaskQueue
  -> AiTask[]
  -> AiContextPack[]
  -> AiRun[]
  -> AiArtifact[]
  -> AiConversation[]
  -> Job[]
```

Основные сущности:

- `AiProvider` - OpenRouter/OpenAI-compatible/local/agent.
- `AiProviderCredentialRef` - secure reference to key/secret.
- `AiTask` - durable background work item.
- `AiTaskClaim` - provider/agent claim with lease.
- `Job` - execution record for task processing with retry/progress/lease.
- `AiContextPack` - selected source chunks and permissions.
- `AiRun` - execution attempt, model, timing, token/cost metadata.
- `AiArtifact` - typed output.
- `AiConversation` - chat/explain-back thread.
- `AiMessage` - conversation turn.
- `AiToolCall` - optional tool/retrieval call metadata.

Task:

```text
AiTask {
  id
  kind
  source_ref
  instruction
  context_policy
  output_schema
  status
  priority
  created_by
  created_at
  updated_at
}
```

Artifact:

```text
AiArtifact {
  id
  task_id
  kind
  payload
  source_refs
  status: draft | accepted | rejected | superseded
  model_info
  created_at
}
```

## Реализация

### Provider abstraction

Define service:

```text
AiProviderClient {
  chat(request) -> stream/messages
  complete_structured(request, schema) -> payload
  transcribe(audio, options) -> transcript
  capabilities() -> ProviderCapabilities
}
```

OpenRouter implementation uses OpenAI-compatible API. Provider-specific fields
stay in `provider_options`, not in core task schema.

### Queue worker

Workers:

- local client worker for BYOK desktop/native session;
- server worker using the user's server-stored BYOK key;
- server worker for app subscription/server-side mode later, outside current
  scope;
- external agent worker via MCP/CLI.

Worker steps:

1. Claim task with lease.
2. Build context pack through explicit source context or indexed retrieval and
   source permissions.
3. Call provider or agent-specific execution.
4. Validate structured output.
5. Write artifact and derived draft objects.
6. Mark task succeeded/failed.

Web AI tasks normally execute through server jobs because web is cloud-backed.
Native clients may execute local/BYOK/local-model jobs and sync resulting
artifacts according to privacy policy.

### MCP interface

Potential MCP tools:

- `lumi_list_ai_tasks`;
- `lumi_claim_ai_task`;
- `lumi_get_ai_task_context`;
- `lumi_write_ai_artifact`;
- `lumi_fail_ai_task`;
- `lumi_search_context`;
- `lumi_create_kb_note`;

Это начальный AI-oriented subset общего MCP surface из [`mcp.md`](mcp.md).
Остальные tools открывают product user operations через те же application
commands.

### Prompt and schema management

Prompts should be versioned:

- task kind;
- prompt template version;
- output schema version;
- model/provider;
- context policy.

This makes generated artifacts auditable and reproducible enough for debugging.

## Интеграции и зависимости

- **Reader.** Creates AI tasks/actions from selected text and current context.
- **Очередь задач.** Background task UX, internal execution и MCP claims
  описаны в [`ai-task-queue.md`](ai-task-queue.md).
- **Саммари.** Summary artifacts, chapter actions and generated `.lum`
  materials описаны в [`ai-summaries.md`](ai-summaries.md).
- **Нормализованный контент.** Supplies bounded explicit source context for
  selection/chapter/material workflows.
- **Search.** Supplies ranked retrieval chunks for open material/library/record
  queries after `SEARCH-005` is available.
- **Learning.** Receives question/card drafts and explain-back feedback.
- **База знаний.** Receives accepted summaries, concepts, note drafts and links.
- **Desk.** Показывает сохраненные typed artifacts вокруг материалов.
  Raw chat messages и промежуточный dialogue state туда не входят.
- **Синхронизация.** Tasks/artifacts/conversations sync as user data; secrets do
  not sync plaintext.
- **Веб-аккаунт.** Web sessions, account-scoped server workers and secret
  storage policy описаны в [`web-account.md`](web-account.md). Облачная реплика
  может быть источником AI context только через явную context policy.
- **Backend/jobs.** AI execution uses the common `Job` infrastructure from
  [`backend-api.md`](backend-api.md), not a separate queue implementation.
- **MCP.** External agent user operations and queue worker tools описаны в
  [`mcp.md`](mcp.md).
- **Security/privacy.** Context policy and data visibility follow
  [`security-privacy.md`](security-privacy.md).
- **Social.** AI can operate on shared content only where permissions allow.
- **Плагины.** Plugins can add providers, task kinds and artifact renderers with
  capabilities.

## Альтернативы

- `accepted`: OpenRouter via OpenAI-compatible API as first provider target.
- `accepted`: durable AI task queue for noninteractive work.
- `accepted`: common `Job` engine for AI execution, import, indexing,
  transcription and repair.
- `accepted`: MCP external agent interface with CLI fallback for simple worker
  automation.
- `accepted`: MCP is an external automation surface with target parity for
  product user operations, кроме admin, credentials/security, chat runtime и
  account deletion.
- `accepted`: server-side secret storage for BYOK in the first web AI slice;
  optional client-local storage remains future native work.
- `accepted`: built-in Lumi subscription is future work outside the current
  implementation scope.
- `rejected`: hardwire one LLM provider into reader UI. This breaks user
  control and replaceability.
- `rejected`: send whole library to AI by default. Too expensive and bad for
  privacy.
- `rejected`: make external agent responsible for all AI. Direct chat and
  explain-back inside Lumi need low-latency provider integration.
- `revisit`: local models. Desirable, but model distribution/runtime is
  separate from core AI task contract.

## Открытые вопросы

- Which OpenRouter model should be default per task type?
- Should AI conversations be indexed by default, or only saved answers/artifacts?
- How should an accepted `abridged_material` relate to source revisions and
  later source updates?
