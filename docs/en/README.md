# English Documentation

This directory is the canonical English documentation tree used for
development.

The matching Russian tree is [`../ru`](../ru). Russian is the preferred working
language for early discussion and source planning when that helps the team move
faster. Durable product, architecture, process and implementation decisions
must be translated and stabilized here before they are used as development
guidance.

The Markdown document set should match [`../ru`](../ru) by relative path. If a
document exists in one language tree, the other language tree should also
contain the same relative path.

Some deep design documents may initially be mirrored from Russian source text
before they are fully translated. Keep the mirror complete, then polish the
English text incrementally as each area becomes active for implementation.

Current documents:

- [`vision.md`](vision.md) - product vision.
- [`early-slices.md`](early-slices.md) - first implementation slices.
- [`system-design/`](system-design/) - accepted `v01` target system design.
- [`adr/`](adr/) - durable architecture decision records.
- [`runbooks/local-dev.md`](runbooks/local-dev.md) - local development workflow.

Temporary implementation plans live outside the language mirror in
[`../tmp-plans`](../tmp-plans). They are tactical working documents for active
intermediate development slices. Durable decisions discovered there should be
promoted back into both language trees, with this English tree as the
development canon.
