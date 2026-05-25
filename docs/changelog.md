# tread — dev changelog

The canonical, repo-visible record of notable work: what changed, *why*,
the key files, test deltas, and the commit. Newest first.

This is the general log. A large multi-part workstream may keep its own
deep-dive doc (e.g. `docs/reading-ui-overhaul.md`) for per-item detail;
this file then carries a one-entry summary pointing at it.

**What counts as notable:** a shipped feature, an architecture/refactor
decision, a behaviour change a teammate would want explained, or anything
whose *why* isn't obvious from the diff. Trivial fixes (typos,
formatting, dependency bumps) don't need an entry.

**Convention:** add the entry in the *same commit* as the change, and
keep it self-contained — `todo.md` and `v2.md` are gitignored, so don't
rely on them for anyone reading a fresh checkout. Mirrors the
"docs in lockstep" rule in `CLAUDE.md`.

**Entry template:**

```
### <date> — <title>
- **Commit:** `<short-hash>`
- **What:** one or two sentences.
- **Why:** the structural reason / decision.
- **Key files:** the files a reader should open first.
- **Tests:** what was added/changed; pass counts or golden deltas.
```

---

### 2026-05-25 — Record-keeping: dev changelog + commit-time guardrail
- **What:** added this changelog, a non-blocking `.githooks/pre-commit`
  reminder, and a CLAUDE.md workflow rule, so notable work is logged in a
  repo-visible place in-commit.
- **Why:** the deep implementation narrative previously lived only in
  agent working memory (not in the repo); `todo.md` / `v2.md` are
  gitignored. Teammates reading a checkout had only commit messages.
- **Key files:** `docs/changelog.md`, `.githooks/pre-commit`, `CLAUDE.md`.
  Enable the hook on a fresh clone with `git config core.hooksPath .githooks`.
- **Tests:** none (docs + tooling).

### Reading-UI overhaul (2026-05) — typography, panes, tables
- **Commits:** `c0165d9`, `9d1d6cd` (six priorities + fixes); `1015adc`
  (paragraph rhythm); `d5523f7` (preview-pane ratio); `cd51f91` (math
  wrapping).
- **What:** a multi-part pass on the reading experience — reading measure,
  heading hierarchy, inline-code/quote styling, contextual preview pane,
  full-screen contents view, table column alignment, paragraph-rhythm
  normalization, adjustable preview-pane ratio, and display-math wrapping.
- **Why / per-item detail:** see the deep-dive at
  [`docs/reading-ui-overhaul.md`](reading-ui-overhaul.md), which carries
  the what/why/files/test-deltas for each item.
- **Remaining:** theme semantic layer, reading-comfort affordances, TOC
  collapse/resize.
