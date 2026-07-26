# Единый план реализации Lumi 0.2.0–0.5.0

Status: `active`

Последнее обновление: 2026-07-26

## Назначение

Этот документ задаёт единственную последовательность выполнения текущих
временных планов. Он не заменяет подробные чек-листы, release gates и
канонические продуктовые решения.

Правила исполнения:

1. В работе находится только один этап.
2. Следующий этап начинается после закрытия gate текущего.
3. Незавершённая возможность остаётся выключенной capability flag.
4. Изменение scope сначала вносится в канонические документы и подробный план,
   затем отражается здесь.
5. После выпуска релиза его временный план архивируется или удаляется, а
   ROADMAP переводится на следующий незавершённый этап.

## Текущая точка

Для `0.2.0` уже завершены:

- Stage 0: spikes, ADR, threat review и release scope;
- Contract Freeze 1: общие AI/MCP contracts, fixtures, mocks и router seams;
- A1: AI persistence, `SecretStore`, общий fenced `Job` runtime и проверенные
  owner/idempotency/transactional publication invariants.

Текущий следующий этап — `0.2.0 / A2: Explicit source context и context packs`.

## Последовательность

### 1. Lumi 0.2.0 — ИИ, саммари, чат и MCP

Подробный план:
[`0.2.0-ai-plan.md`](0.2.0-ai-plan.md).

1. [x] Stage 0 — решения, spikes и threat review.
2. [x] Contract Freeze 1 — общие contracts и fixtures.
3. [x] A1 — AI persistence, `SecretStore` и общий `Job` runtime.
4. [ ] A2 — bounded explicit source context и context packs.
5. [ ] A3 — BYOK settings и OpenRouter provider.
6. [ ] B1 — Web AI shell, settings и fixture-backed states.
7. [ ] B2 — durable conversations и streaming.
8. [ ] A4 — durable AI queue и summary execution.
9. [ ] B3 — Reader actions, queue и summary UX.
10. [ ] C1 — MCP connections, transport и auth.
11. [ ] C2 — MCP product и AI worker tools.
12. [ ] C3 — сокращённый `.lum` и derived-material flow.
13. [ ] Release gate — интеграция, hardening, migration и staging evidence.

Результат: один проверенный Web/MCP AI-контур с общими task, artifact,
context и authorization contracts.

### 2. Lumi 0.3.0 — обучение после чтения

Начинается только после release gate `0.2.0`.

Подробный план:
[`0.3.0-learn-plan.md`](0.3.0-learn-plan.md).

1. [ ] G0 — prerequisite evidence, ADR, contracts и fixtures.
2. [ ] C0 — UX states, routing и fixture prototype.
3. [ ] G1 — shared learning foundation.
4. [ ] A1 — durable learning CRUD, sessions и attempts.
5. [ ] A2 — completion и immediate recall.
6. [ ] C1 — deterministic Reader/session vertical.
7. [ ] A3 — FSRS, hints и due projections.
8. [ ] C2 — Challenges и scheduling UX.
9. [ ] B1 — generation и open-answer evaluation.
10. [ ] B2 — text explain-back.
11. [ ] C3 — AI drafts и explain-back UX.
12. [ ] B3 — audio attachments и transcription.
13. [ ] C4 — browser recorder и transcript UX.
14. [ ] A4 — MCP learning parity.
15. [ ] Release gate — hardening, runbook и acceptance.

Результат: deterministic learning, scheduling, AI explain-back и voice flow
закрыты последовательно отдельными вертикальными gates.

### 3. Lumi 0.4.0 — записи, Desk, поиск и RAG

Начинается только после release gate `0.3.0`.

Подробный план:
[`0.4.0-notes-and-desk-plan.md`](0.4.0-notes-and-desk-plan.md).

1. [ ] Gate 0 — ADR, spikes, shared DTO и fixtures.
2. [ ] A1 — Annotation v2 и migration.
3. [ ] A2 — Rich Reader.
4. [ ] A3 — Voice Notes.
5. [ ] A4 — links и backlinks.
6. [ ] B1 — BM25 + fastText foundation.
7. [ ] B2 — Desk projection и surface.
8. [ ] B3 — search surfaces.
9. [ ] B4 — MCP Desk/search parity.
10. [ ] C1 — record context contract.
11. [ ] C2 — record RAG integration.
12. [ ] Gate I — сквозная Reader → Desk → Search → Chat интеграция.
13. [ ] Release gate — rebuild, performance, security и migration evidence.

Результат: единая модель записей проходит через Reader, Desk, поиск, MCP и
record-scoped RAG без второго контура данных.

### 4. Lumi 0.5.0 — социальные пространства

Начинается только после release gate `0.4.0`.

Подробный план:
[`0.5.0-social-plan.md`](0.5.0-social-plan.md).

1. [ ] Gate 0 — social contracts, permission matrix и ADR.
2. [ ] C1 — matching/anchor fixtures, risk spikes и multi-account harness.
3. [ ] A1 — domain, persistence, jobs и capabilities foundation.
4. [ ] B1 — Community shell и contract-backed states.
5. [ ] A2 — Spaces, membership и invite access.
6. [ ] B2 — Spaces и membership UX.
7. [ ] C2 — access/security gate.
8. [ ] A3 — material sharing, fingerprints и matching.
9. [ ] B3 — share и material-claim UX.
10. [ ] C3 — sharing/matching security gate.
11. [ ] A4 — shared anchors, discussions и published highlights.
12. [ ] B4 — Reader social layer.
13. [ ] C4 — shared-reading quality gate.
14. [ ] A5 — Space chat, activity, social search events и MCP parity.
15. [ ] B5 — chat и activity UX.
16. [ ] C5 — release hardening.

Результат: два аккаунта проходят полный закрытый Community Space flow без
раскрытия source-файлов и личных записей.

## Финальная точка

Roadmap завершён, когда release evidence `0.5.0` закрыт, критерии завершения
всех четырёх планов выполнены, `make c` и обязательные Web E2E проходят, а
долгоживущие решения перенесены из `docs/tmp-plans/` в канонические документы.
