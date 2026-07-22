# Architecture Decision Records

ADRs capture durable implementation decisions. Use ADRs for the decision classes
listed in [`../systems/quality.md`](../systems/quality.md),
including schema, anchor, sync,
plugin, AI, search and account/auth boundaries.

Текущие source/reader decisions:

- [`0009`](0009-source-backed-anchor-v2.md) — source-backed anchors;
- [`0010`](0010-web-telegram-source-baseline.md) — общий Web/Telegram baseline.
- [`0011`](0011-beta-operations-and-telegram-webhook.md) — beta operations и
  историческая webhook boundary;
- [`0012`](0012-embedded-telegram-bot-settings.md) — встроенный Telegram
  listener и настройка bot token через UI.

## Template

```markdown
# ADR NNNN: Title

Status: proposed | accepted | rejected | superseded

## Context

What problem or boundary forced the decision?

## Decision

What did we choose?

## Consequences

What gets easier, harder or constrained?

## Alternatives

What did we reject and why?

## Compatibility

What migrations, fixtures or tests are required?
```
