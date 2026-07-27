# ADR 0023: MCP Streamable HTTP, auth и tool contracts

Status: accepted

## Контекст

Внешний агент должен работать с application services Lumi и исполнять те же
`AiTask`, не получая browser session, provider secrets, admin control plane или
chat runtime. До production tools нужно выбрать transport, token lifecycle,
limits и fencing. Нефиксированный MCP profile создаст несовместимые clients и
неясную security boundary.

## Решение

- Первый hosted/self-hosted profile использует MCP Streamable HTTP на
  `POST /mcp`, JSON-RPC 2.0 и protocol snapshot `2025-06-18`. Stateless request
  mode является baseline; optional GET/SSE session stream не требуется для
  `0.2.0`.
- Каждый HTTP request передает `Authorization: Bearer`; token в query string,
  cookie или tool arguments запрещен. Server проверяет token до parsing tool
  payload.
- `0.2.0` использует созданный в authenticated Web settings криптографически
  случайный opaque token. Dynamic OAuth authorization-server flow остается
  additive следующим profile; endpoint уже действует как audience-bound
  resource server.
- Token показывается один раз. PostgreSQL хранит account-scoped connection,
  keyed verifier, display prefix, created/last-used/revoked timestamps и
  revision. Проверка каждого request читает актуальный revocation state;
  process-local positive cache не переживает revoke.
- Один token дает product-user permissions одного personal account без admin,
  credentials/security, billing, account deletion и global chat. Per-tool и
  read-only grants не входят в первый срез.
- Обязательные protocol methods: `initialize`, `notifications/initialized`,
  `tools/list`, `tools/call`. `tools/list` объявляет только реально enabled
  capabilities; первым probe tool является `get_lumi_capabilities`.
- Tool names и JSON Schemas versioned через committed snapshots, но отдельный
  runtime schema registry не вводится. Mutations имеют idempotency key,
  conflict updates — expected revision, lists/chunks — cursor pagination.
- AI claim result содержит `task_id`, `run_id`, `claim_id`, монотонный `fence`,
  `lease_expires_at`, task revision и result schema version. Progress,
  heartbeat, complete, fail и release обязаны вернуть эти fencing fields.
- Control request не больше 1 MiB, обычный tool result — 256 KiB, list page —
  максимум 100 items. Большие files/results идут через bounded upload ref с
  owner, media type, checksum, size, expiry и single-purpose consumption.
- Control call timeout — 30 секунд; async import/export/AI возвращает id.
  Rate/concurrency limits применяются на connection и account. Errors
  типизированы и не содержат content bodies/internal SQL/provider details.

## Последствия

- MCP может масштабироваться без sticky in-memory sessions.
- Revocation действует на следующий request; уже claimed AI run отдельно
  теряет lease/claim по cancel/revoke policy и не может завершиться без fence.
- HTTP/Web и MCP adapters вызывают одни application commands и authorization.
- Local stdio bridge позже может проксировать этот же tool contract, но не
  становится отдельной business implementation.

## Альтернативы

- Legacy HTTP+SSE transport как единственный profile: отклонено в пользу
  Streamable HTTP.
- Browser session cookie: отклонено из-за CSRF/session coupling.
- Token plaintext в PostgreSQL: отклонено.
- Stateful MCP session как authorization state: отклонено; auth проверяется на
  каждом request.
- Универсальный `execute_action`: отклонено в пользу typed tools.

## Совместимость и проверки

- Protocol/tool snapshots находятся в `tests/fixtures/mcp/contracts/v1/`.
- Contract/security tests проверяют initialize/list/call, missing/invalid/
  revoked token, cross-account ids, body/result limits, stale claim, duplicate
  completion и запрет credential/admin/chat tools.
- Исполняемый transport/revocation probe находится в
  `spikes/stage0/src/mcp.rs`.
