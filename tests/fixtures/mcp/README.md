# MCP fixtures

`contracts/v1/` фиксирует MCP protocol/tool snapshots для первого
account-scoped vertical slice. Snapshot использует Streamable HTTP,
JSON-RPC 2.0 и MCP protocol version `2025-06-18`.

Tokens, verifier hashes и private content в fixtures запрещены. Claim examples
содержат обязательные `run_id`, `claim_id`, `fence`, lease и task revision.
