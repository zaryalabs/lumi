# TMP Plans

Status: `active`

`TMP Plans` is a temporary documentation section for intermediate
implementation plans. These documents turn an already designed product slice
into concrete implementation work, but they are not the canonical source of
product or architecture decisions.

Use this directory when a slice is already designed enough, but development
still needs a staged plan, task breakdown, release gate or temporary
coordination document.

## Rules

- Every temporary plan must link to the product or architecture documents it
  implements.
- Every temporary plan must have a status, scope and completion criteria.
- Temporary plans are written in Russian while the repository uses Russian as
  its single documentation language.
- If a temporary plan records a durable product, architecture or process
  decision, that decision must be promoted into the canonical documentation
  under `docs/`.
- [`ROADMAP.md`](ROADMAP.md) is the temporary execution order for the active
  plans. It must not introduce product or architecture decisions; long-lived
  sequencing stays in the relevant canonical product or systems documents.
- Work is sequential at product-epic level: only one epic from `ROADMAP.md`
  may be active. Backend, Web, tests, documentation and bounded spikes inside
  that epic may proceed as separate workstreams after their shared contract is
  fixed.
- Detailed A/B/C packages and local gates inside a release plan are ownership
  maps and acceptance checklists, not separate ROADMAP checkpoints.
- A product epic closes only when its end-to-end outcome, migrations, tests,
  documentation and required common quality gate are complete.
- After a slice ships or is superseded, archive, replace or delete its
  temporary plan.

## Current Plans

- [`ROADMAP.md`](ROADMAP.md) — 18 крупных продуктовых эпиков от текущего
  состояния до завершения основного Web roadmap `0.5.0`.

- [`0.2.0-ai-plan.md`](0.2.0-ai-plan.md) — реализация BYOK, глобального
  ИИ-чата, саммари, durable AI task queue и MCP-интеграции для внешних агентов.

- [`0.5.0-social-plan.md`](0.5.0-social-plan.md) — Community Spaces,
  совместное чтение, comments, shared highlights и chat.

Допустимые status для планов:

- `draft` — scope и durable решения ещё обсуждаются;
- `planned` — зависимости и release scope согласованы, реализация не завершена;
- `active` — по плану идёт production-реализация;
- `completed` — release evidence закрыт, durable решения перенесены в
  канонические документы.

Архив завершённых планов:

- [`0.3.0-learn-plan.md`](../archive/0.3.0-learn-plan.md) — закрытый
  repository-side выпуск обучения после чтения.
- [`0.4.0-notes-and-desk-plan.md`](../archive/0.4.0-notes-and-desk-plan.md) —
  закрытый repository-side выпуск записей, Desk, поиска и record-scoped RAG.
