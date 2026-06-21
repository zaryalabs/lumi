# ИИ-функционал

Status: draft

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
- использовать встроенную подписку/серверный provider позже;
- отключить ИИ;
- подключить внешнего агента, который будет обрабатывать очередь задач.

Первичный provider target: OpenRouter через OpenAI-compatible API. Архитектура
не должна быть жестко привязана к OpenRouter, потому что будущие providers,
локальные модели и external agents должны использовать тот же task/artifact
contract.

## Пользовательские сценарии

- Пользователь выделяет абзац и спрашивает "объясни проще".
- Пользователь задает вопрос в чате по книге, главе или всей библиотеке.
- Пользователь запускает "сделай карточки по главе"; задача попадает в очередь,
  а результат появляется позже.
- Пользователь не добавил API key. External agent читает очередь и записывает
  summaries/questions обратно как artifacts.
- Пользователь работает в web-клиенте. BYOK key может быть session-local или
  stored через web-account secret policy; server-side subscription mode может
  читать облачную реплику только по явной context policy.
- Пользователь запускает explain-back внутри Lumi. Это работает только при
  direct AI provider/key/subscription.
- Пользователь просит external agent провести explain-back. Agent открывает
  собственный UI и возвращает final artifact/attempt summary to Lumi.
- Пользователь выбирает, какие AI artifacts принять в KB/learning.

## Функциональные требования

### AI task queue

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

### Interactive chat

Chat differs from background tasks:

- user expects streaming/low-latency response;
- context may be selected from current material, library search, KB or explicit
  attachments;
- conversation history is stored as `AiConversation`;
- user can choose to save an answer as KB note or artifact.

Chat inside Lumi requires available provider. External agent may implement its
own chat UI, but Lumi cannot display live turns without provider integration.

### Selection actions

Reader actions:

- explain selected text;
- summarize section;
- ask about selection;
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

### Retrieval context

ИИ не должен получать entire library by default. AI layer вызывает search
retrieval:

```text
AiRequest
  -> scope/context policy
  -> search.retrieve(...)
  -> context pack
  -> provider/agent
  -> artifact/conversation response
```

Context pack stores citations и hashes, чтобы results могли ссылаться на
sources.

### Provider model

Provider interface:

- OpenAI-compatible chat/completions for OpenRouter first.
- Structured output where possible for questions/cards/entities.
- Streaming for chat and explain-back.
- Batch/background calls for tasks.
- Provider capability metadata: max context, supports JSON schema, supports
  audio, supports embeddings, supports vision if ever needed.

Secrets:

- API keys are stored in secure local/server secret storage.
- Keys are not synced as plaintext.
- External agent credentials stay outside ordinary sync.

### External agent integration

Primary design: queue is exposed through MCP-like interface and optional CLI
worker fallback.

Agent capabilities:

- list tasks;
- claim task;
- read allowed context;
- write artifact/result;
- mark failed with reason;
- attach files if needed.

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

This preserves the learning value without pretending Lumi can host live agent UI
without model access.

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
- **Privacy.** Context inclusion is explicit and logged.
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
- server worker for app subscription/server-side mode later;
- external agent worker via MCP/CLI.

Worker steps:

1. Claim task with lease.
2. Build context pack through search/retrieval and source permissions.
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

Exact tool schema will be designed when implementing external agent bridge.

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
- **Search.** Supplies retrieval chunks for chat/tasks.
- **Learning.** Receives question/card drafts and explain-back feedback.
- **База знаний.** Receives accepted summaries, concepts, note drafts and links.
- **Синхронизация.** Tasks/artifacts/conversations sync as user data; secrets do
  not sync plaintext.
- **Веб-аккаунт.** Web sessions, account-scoped server workers and secret
  storage policy описаны в [`web-account.md`](web-account.md). Облачная реплика
  может быть источником AI context только через явную context policy.
- **Backend/jobs.** AI execution uses the common `Job` infrastructure from
  [`backend-api.md`](backend-api.md), not a separate queue implementation.
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
- `accepted`: MCP-like external agent bridge with CLI fallback.
- `rejected`: hardwire one LLM provider into reader UI. This breaks user
  control and replaceability.
- `rejected`: send whole library to AI by default. Too expensive and bad for
  privacy.
- `rejected`: make external agent responsible for all AI. Direct chat and
  explain-back inside Lumi need low-latency provider integration.
- `revisit`: local models. Desirable, but model distribution/runtime is
  separate from core AI task contract.

## Открытые вопросы

- Store BYOK credentials only locally or allow encrypted server-side storage for
  web sessions?
- Which OpenRouter model should be default per task type?
- How strict should context logging be for privacy/audit?
- What exact MCP schema should external agents use?
- Should AI conversations be indexed by default, or only saved answers/artifacts?
