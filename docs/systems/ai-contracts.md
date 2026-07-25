# AI/MCP Contract Freeze 1

Status: accepted

## Назначение

Этот документ фиксирует минимальную общую поверхность `0.2.0`, после которой
треки AI Platform, Chat & Product UX и Agent & Derived Content могут работать
независимо. Контракт не означает, что соответствующие routes, persistence,
worker или capabilities уже включены.

Domain schema marker после freeze:
`s1.2026-07-25.ai-contract-v1`.

Основные version ids:

- task/API contract — `ai.contract.v1`;
- context pack — `ai-context-pack.v1`;
- citation — `source-citation.v1`;
- context limits — `explicit-context-limits.v1`;
- summary output — `summary-artifact.v1`;
- MCP tools — `mcp-tools.v1`;
- MCP protocol — `2025-06-18`.

## Канонические источники контракта

- `crates/lumi-core/src/ai.rs` — ids, states, task/run/chat/artifact DTO,
  commands, validators, context/citation/provider events, HTTP route catalog,
  prompt и output schema registry;
- `crates/lumi-core/src/mcp.rs` — connection DTO, tool allowlist, claim fencing,
  complete/fail/progress contracts и management route catalog;
- `tests/fixtures/ai/contracts/v1/` — JSON snapshots AI/API/schema registry;
- `tests/fixtures/mcp/contracts/v1/` — protocol, tool registry и worker claim
  snapshots;
- `crates/lumi-server/src/ai/providers.rs` — provider-neutral interface;
- `crates/lumi-server/src/ai/mock.rs` — общий deterministic mock provider и
  mock task application service.

Rust contract tests читают committed JSON snapshots напрямую. Breaking
изменение создает новый version id/directory; редактирование `v1` на месте
запрещено.

## Frozen state machines

Пользовательский `AiTaskStatus`:

```text
queued -> running -> succeeded
                 \-> needs_input
                 \-> failed
queued/running ----> cancelled
running lease ------> queued
needs_input/failed -> queued
```

`claimed` не является состоянием task. Технический run использует
`pending | running | succeeded | failed | cancelled | released`.

Artifact lifecycle:
`candidate | active | rejected | superseded`.

Validators запрещают неизвестные prompt/output versions, пустые revision-bound
scope, mismatched selection anchor, oversized context/result, denied permission
snapshot, duplicate/unbound citation ids и terminal task transitions.

## HTTP contracts

Полный method/path/request/response catalog зафиксирован в
`tests/fixtures/ai/contracts/v1/http-routes.json` и проверяется против Rust
catalog. Он включает:

- provider state, write-only credential, preferences и validation;
- conversations, messages, generation events/stop/retry/regenerate;
- tasks, execute/cancel/retry/bulk;
- artifacts accept/reject;
- summary/abridgement commands;
- MCP connection list/create/revoke/rotate.

Control plane использует JSON, cursor pages и optimistic revisions.
Создающие/повторяемые mutations несут idempotency key. Secret присутствует
только в `PutProviderCredentialRequest`; его `Debug` representation всегда
redacted, а read DTO secret-поля не имеют.

## MCP tools и fencing

Allowlist `0.2.0` зафиксирован одновременно константой `MCP_TOOL_NAMES` и
snapshot `tool-registry.json`. Каждый tool имеет отдельные input/output schema
version ids. `search` в allowlist не входит.

Claim result обязательно содержит:

```text
task_id
run_id
claim_id
fence
lease_expires_at
task_revision
result_kind
result_schema_version
context_pack_id
```

`get context`, progress, complete, fail и release передают общий набор
`task_id + run_id + claim_id + fence + task_revision`. Completion дополнительно
содержит idempotency key и typed result, который проходит output schema
validator до публикации.

## Router composition

`crates/lumi-server/src/api_routes.rs` теперь является короткой integration
boundary. Existing product routes, AI contribution и MCP management
contribution объединяются отдельными routers. Top-level `/mcp` также подключен
через отдельный пустой до C1 contribution.

Track B добавляет conversation routes в `ai/chat.rs`; Track A расширяет AI
route contribution своими modules; Track C владеет `mcp/`. Изменение
top-level composition для обычного добавления route больше не требуется.

## Ownership

- Track A владеет `lumi-core` AI contracts после freeze, provider/task runtime,
  base AI migrations и server AI modules, кроме `chat.rs`;
- Track B владеет `ai/chat.rs`, conversation migration и `apps/web/src/ai/*`;
- Track C владеет `lumi-server/src/mcp/*`, MCP migration и derived content;
- integration owner владеет root/workspace `Cargo.toml`,
  `crates/lumi-server/src/lib.rs`, `api_routes.rs`, capabilities и top-level
  Web navigation.

Integration owner — назначенный maintainer текущей точки Sync/Release. Track не
меняет integration-owned file в одностороннем порядке: он передает отдельный
малый integration patch владельцу. Frozen DTO меняется только отдельным
contract change с обновлением mocks/snapshots и явным согласованием A, B и C.

## Capability rule

Freeze публикует типы, schemas и test doubles, но не включает
`ai-*`/`mcp-*` capabilities. Capability объявляется только после готовности
route, persistence/worker и минимальных contract/security tests.
