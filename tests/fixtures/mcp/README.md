# MCP fixtures

`contracts/v1/` фиксирует MCP protocol/tool snapshots для первого
account-scoped vertical slice. Snapshot использует Streamable HTTP,
JSON-RPC 2.0 и MCP protocol version `2025-06-18`.

`contracts/v2/` добавляет learning parity, `contracts/v3/` — Desk/search,
а `contracts/v4/` — Community
Spaces, material discussions и Space chat поверх тех же application services.

`tool-registry.json` является frozen allowlist с отдельными version ids
input/output schemas. AI worker mutations обязаны использовать общий набор
`task_id + run_id + claim_id + fence + task_revision`; complete дополнительно
требует idempotency key и typed result.

Tokens, verifier hashes и private content в fixtures запрещены. Claim examples
содержат обязательные `run_id`, `claim_id`, `fence`, lease и task revision.
