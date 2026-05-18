# Architecture Decision Records

Decisions that shaped the current shape of `tread`. ADRs capture the
why behind a structural choice so future work doesn't re-litigate
settled ground — and so an audit can tell "deliberate" from "accidental"
at a glance.

Companions: [`CONTEXT.md`](../../CONTEXT.md) (domain vocabulary) and
[`CLAUDE.md`](../../CLAUDE.md) (invariants and edit recipes).

## Index

| # | Title | Status |
|---|---|---|
| [0001](0001-figure-model.md) | Figure model | Accepted |
| [0002](0002-preview-pane-model.md) | Preview pane model | Accepted |
| [0003](0003-terminal-image-strategy.md) | Terminal image strategy | Accepted |

## Format

Each ADR is one file: `NNNN-kebab-title.md`. Numbers are sequential and
never reused. The skeleton:

```markdown
# ADR-NNNN — Title

- **Status:** Proposed | Accepted | Deprecated | Superseded by ADR-NNNN
- **Crate(s):** affected crate / module list
- **Relates to / Supersedes:** optional cross-links

## Context
The forces in play — what made this question come up, what failed in
earlier approaches, what constraints are non-negotiable.

## Decision
What we actually do now. Concrete enough that a contributor can verify
the codebase matches the ADR.

## Consequences
Good and costs. Honest about the costs — a decision with no downside
is usually under-described.

## Validation
How we keep the decision honest: tests, smoke runs, manual sweeps.

## Open follow-up
Known loose ends. If something is on a deepening backlog, name it here
rather than letting it drift.
```

## When to write one

Write an ADR when:

- A choice has structural reach — multiple modules, the embed surface,
  or the parse/render/image boundary.
- A choice fixed a previous mistake that would be easy to re-introduce.
- A choice traded off two reasonable alternatives and the reasoning
  isn't visible in the diff.

Don't write one for:

- A local refactor with no API impact.
- A bug fix where the commit message carries the why adequately.
- Style or formatting choices.

## When to update one

ADRs are versioned by status, not by edit. If the decision still holds
but details have moved, edit in place. If the decision no longer
holds, mark the old ADR `Superseded by ADR-NNNN` and write a new one
that explains what changed.
