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
- Work is sequential: only one stage from `ROADMAP.md` may be active. The next
  stage starts only after the current stage gate is recorded as passed.
- After a slice ships or is superseded, archive, replace or delete its
  temporary plan.

## Current Plans

- [`ROADMAP.md`](ROADMAP.md) — единая последовательность работ от текущего
  состояния до завершения `0.5.0`.

- [`0.2.0-ai-plan.md`](0.2.0-ai-plan.md) — реализация BYOK, глобального
  ИИ-чата, саммари, durable AI task queue и MCP-интеграции для внешних агентов.

- [`0.3.0-learn-plan.md`](0.3.0-learn-plan.md) — реализация обучения после
  чтения: post-reading tests, FSRS reviews, подсказки, voice answers и
  explain-back.

- [`0.4.0-notes-and-desk-plan.md`](0.4.0-notes-and-desk-plan.md) — богатые
  записи по чтению, Desk, BM25 + fastText поиск и RAG по записям.

- [`0.5.0-social-plan.md`](0.5.0-social-plan.md) — Community Spaces,
  совместное чтение, comments, shared highlights и chat.

Допустимые status для планов:

- `draft` — scope и durable решения ещё обсуждаются;
- `planned` — зависимости и release scope согласованы, реализация не завершена;
- `active` — по плану идёт production-реализация;
- `completed` — release evidence закрыт, durable решения перенесены в
  канонические документы.
