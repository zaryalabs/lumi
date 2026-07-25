# Threat review AI и MCP для `0.2.0`

Status: accepted

## Контекст и границы

Review покрывает cloud-backed Web scope `0.2.0`: BYOK OpenRouter,
server-backed chat, explicit source context, durable AI tasks/artifacts,
account-scoped MCP и generated `.lum`. Встроенная подписка, local models,
client-local execution, arbitrary provider endpoints, plugins и indexed
library-wide retrieval не входят в эту границу.

Защищаемые активы:

- provider credential и MCP bearer token;
- private materials, annotations, conversations, context packs и artifacts;
- account/application authorization boundary;
- task claim, artifact publication и derived-material integrity;
- availability worker/provider/MCP/API;
- redacted operational logs, traces, metrics и backups.

Недоверенные входы:

- любой imported/source text, включая инструкции для модели;
- user prompt и AI/provider output;
- OpenRouter HTTP/SSE/error payload;
- MCP client, tool arguments и uploaded result;
- generated `.lum` container/Markdown/provenance;
- ids, anchors, cursors и idempotency keys клиента.

## Решения review

| Угроза | Attack path и impact | Обязательная защита `0.2.0` | Проверка / residual risk |
| --- | --- | --- | --- |
| Secret exfiltration | prompt, Web client, MCP tool, provider error или diagnostics пытаются прочитать BYOK/MCP token | reusable AEAD `SecretStore`; credential никогда не входит в context/tool DTO; token показывается один раз; redacted types/headers; allowlist diagnostics | plaintext scan database/log/API/MCP/browser storage; provider все равно видит отправленный context и пользователь обязан видеть preview/policy |
| Prompt injection | imported text просит раскрыть secrets, расширить scope, вызвать tool или подменить policy | source fragments маркируются как quoted untrusted data; system policy и tool authorization не строятся из model output; chat в `0.2.0` не имеет privileged tools; MCP authorization выполняется до/после AI | malicious fixture требует игнорировать policy и запросить другую account id; семантическое качество ответа остается model risk |
| Cross-account context leak | подмена material/revision/anchor/task ids, неверный join/cache или reused pack | owner-prefixed repository queries; exact revision; permission check build/read/publish; cache key включает account; pack принадлежит account/task/run; errors не подтверждают существование foreign id | PostgreSQL/API/MCP tests со смешанными ids и concurrent accounts; server compromise вне application isolation остается cloud-mode risk |
| Stale MCP claim | агент завершает task после lease expiry, revoke, cancel или нового worker claim | `run_id + claim_id + monotonic fence + lease + task revision` на каждой mutation; transactional complete; revoke блокирует новые calls; cancel проверяется перед publication | race tests stale heartbeat/complete/duplicate; уже отправленный provider request может потреблять внешний бюджет до cancellation |
| SSRF/provider endpoint | пользователь/agent подменяет provider base URL, redirect или model-supplied URL | `0.2.0` OpenRouter base URL compile/config allowlist; account API не принимает endpoint; HTTPS; redirect/DNS policy; model ids являются catalog ids, не URLs; existing import SSRF policy остается общей | tests private/link-local/metadata/redirect targets; self-hosted operator может явно поменять deployment config и принимает этот риск |
| Oversized output/context | provider/MCP возвращает бесконечный stream, JSON/ZIP bomb или огромный result | request/fragment/pack/output byte limits; streaming счетчик и deadline; cancellation; strict schema depth/item/string limits; large result через bounded blob ref; ordinary constrained `.lum` validation | mock provider malformed/slow/oversize, MCP 413/result limit, ZIP corpus; provider может списать бюджет до локального cutoff |
| Log leakage | prompt/context/output/credential попадает в tracing, panic, raw error или debug flag | metadata-only spans; body/header denylist; no OpenRouter debug in production; canonical error mapping; secret redacted `Debug`; content hashes не заменяют secret redaction | captured-log fixtures по success/error/cancel; operator-level packet capture/core dump вне app logging policy |
| Malicious structured output | artifact содержит extra fields, invalid anchors, HTML/script или чужие ids | strict versioned JSON Schema, `additionalProperties: false`, semantic owner/source validation, safe Markdown/renderers, artifact publish только после validation | schema/anchor/cross-account fixtures; factual correctness требует citations/user judgment |
| Derived-package injection | agent возвращает path traversal, active content, false provenance или partial package | agent возвращает typed chapter/result либо bounded upload ref; Lumi собирает/проверяет constrained `.lum`; provenance ids re-authorized; atomic ordinary import/publication | malicious `.lum` corpus и absence-before-success test; provenance не доказывает factual faithfulness |
| Replay/idempotency abuse | повтор create/complete/import создает расходы или duplicate results | scoped idempotency keys + canonical request/result hash; active task dedupe; duplicate complete returns existing result only for same hash/fence | replay fixtures; пользователь может намеренно создать новую task с новым key |
| Resource exhaustion | много chat streams, claims, heartbeats или bulk tasks исчерпывают workers/provider quota | per-account/connection concurrency/rate limits, bounded bulk size, admission before claim, provider timeout, retry budget/backoff, queue fairness | load/budget tests на hardening; exact production quotas настраиваются deployment profile |
| Citation spoofing | model/agent указывает несуществующую или чужую citation | result принимает только citation ids из immutable context pack; resolver повторно проверяет revision/anchor; UI не делает произвольный URL кликабельным source ref | unknown/foreign/stale citation fixtures; citation подтверждает источник, не вывод |

## Security invariants для implementation review

1. Ни один DTO, доступный Web/MCP, не содержит decrypt/read operation для
   provider credentials.
2. Authorization не зависит от prompt, model output, provider или MCP-supplied
   account id.
3. Context pack не дает больше прав, чем актуальная permission check.
4. Ни один stale/отозванный claim не может изменить task/run/artifact.
5. Structured output и generated package не публикуются частично.
6. Provider endpoint нельзя задать через account/UI/MCP в `0.2.0`.
7. Content bodies отсутствуют в default logs/traces/metrics.
8. Capability объявляется только вместе с route, persistence, authorization,
   limits и минимальными contract/security tests.

## Required fixtures и gates

Fixture catalog закреплен в
[`../tmp-plans/0.2.0-stage0-spikes.md`](../tmp-plans/0.2.0-stage0-spikes.md) и
`tests/fixtures/{ai,mcp}`. До release обязательны:

- database/log/browser/MCP plaintext scans;
- cross-account matrix для каждой AI/MCP repository query;
- prompt-injection corpus;
- OpenRouter malformed/mid-stream error/timeout/oversize/cancel corpus;
- MCP revoke, stale fence, replay и body/result limit corpus;
- generated `.lum` path/size/provenance/partial-publication corpus;
- backup/restore drill encrypted envelopes без раскрытия secrets.

Незакрытый high/critical finding блокирует capability и release. Medium finding
допускается только с owner, сроком, feature flag по умолчанию `off` и
документированным residual risk.

## Связанные решения

- [ADR 0019](../adr/0019-ai-task-run-artifact-schema.md);
- [ADR 0021](../adr/0021-provider-secret-store.md);
- [ADR 0022](../adr/0022-explicit-source-context.md);
- [ADR 0023](../adr/0023-mcp-streamable-http-auth-tools.md);
- [ADR 0024](../adr/0024-derived-material-provenance.md).
