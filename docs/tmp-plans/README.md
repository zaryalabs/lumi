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
- Do not use this section as the final roadmap. Long-lived sequencing should
  stay in `docs/early-slices.md` or the relevant canonical systems documents.
- After a slice ships or is superseded, archive, replace or delete its
  temporary plan.

## Current Plans

- [`playwright-agent-inspection.md`](playwright-agent-inspection.md) - local
  browser inspection notes for the Dioxus Web shell and reader surface.
- [`s1-web-reader.md`](s1-web-reader.md) - staged implementation plan
  from the current scaffold and UI/UX prototype to a working S1 Web EPUB
  Reader.
