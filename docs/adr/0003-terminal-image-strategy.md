# ADR-0003 — Terminal image strategy

- **Status:** Accepted (2026-05-18)
- **Crates:** `tread::images`, `kitty-graphics`
- **Relates to:** [ADR-0001 — Figure model](0001-figure-model.md),
  [ADR-0002 — Preview pane model](0002-preview-pane-model.md)

## Context

We want to render arXiv figures as actual pixels — not text placeholders —
on terminals that support it. Three properties of the environment shape
the design:

1. **ratatui has no notion of pixel data.** Its frame buffer is a grid
   of character cells. Embedding a Kitty graphics escape in a `Span`
   corrupts cell-width accounting and produces visual garbage.
2. **Two host terminals dominate, and they disagree.** Native Kitty (and
   WezTerm, Ghostty) keep a server-side image store: transmit once with
   `a=T,t=t,i=<id>`, then place repeatedly with the cheap `a=p,i=<id>`
   (~50 bytes). iTerm2's Kitty implementation has no persistent store —
   every frame must re-transmit the full base64 payload (~200–400 KB
   per figure).
3. **Users live in tmux.** Tmux doesn't forward escape sequences unless
   `allow-passthrough on` is set, and even then it only forwards
   ≤ ~24 KB per APC sequence reliably. Long payloads must be chunked
   (`m=1` continuation, `m=0` terminator).

Earlier attempts inlined images via terminal-specific protocols mixed
into the render path. The render pass was no longer a pure character-
cell write — debugging visual drift required reasoning about both the
ratatui buffer state and an out-of-band escape stream interleaved with
it.

## Decision

### Images live outside the ratatui frame buffer

`build_visual_lines` emits `VisualLineKind::Image { kitty_id, cols, rows,
is_first }` for image rows. The renderer paints **blank cells** there —
the buffer never sees pixel data. After `terminal.draw()` returns, the
host calls `tread::after_draw` (standalone `ReaderRuntime` does this
internally; embedders call it explicitly). That function walks the
visible window for Image VLs and writes Kitty `a=p` placements directly
to stdout at the same `(row, col, cols, rows)` cells ratatui blanked.

OSC 52 yank uses the same pattern. It is the standard escape hatch in
this codebase for "I need to talk to the terminal in a way ratatui
can't."

### `ImageState` is the public seam

```rust
pub struct ImageState {
    bytes: HashMap<u32, Option<Vec<u8>>>,
    prev_visible: HashSet<u32>,
    last_emitted: HashMap<u32, (u16, u16, u16, u16)>,
    negative_loads: HashMap<u32, Instant>,
    transmitted_ids: HashSet<u32>,
    preview_ids: HashSet<u32>,
    worker: Option<ImageWorker>,
    pending_jobs: HashSet<u32>,
}
```

Public to the embed surface — `trench` constructs one and hands it to
`after_draw`. Embedders treat it as opaque; private implementation
modules (worker scheduling, byte loading, PNG normalization, Kitty
emission, placement, trace logging) live behind this interface. The
split into private submodules is on the deepening backlog — `ImageState`
itself is stable.

### Three caches with different lifetimes

| Cache | Lifetime | Purpose |
|---|---|---|
| `bytes` | session | PNG bytes by `kitty_id`. `Some(_)` = loaded, `None` = load failed. |
| `transmitted_ids` | session, terminal-cache-permitting | Ids the *terminal* has cached. Empty on iTerm2. |
| `last_emitted` | per frame | `(row, col, cols, rows)` of the most recent placement. Equal to current frame's intent ⇒ skip emission. |

`last_emitted` is the big win: an idle frame (focus events, mouse
motion, key repeats that don't change scroll position) compares the
intended placement against the previous one and skips the entire
delete+transmit cycle for each unchanged figure. ~210 KB of base64 per
visible figure per idle frame becomes ~0.

`transmitted_ids` is the second-tier win: on native Kitty, an already-
transmitted image can be re-placed with `a=p,i=<id>` (~50 bytes) on
scroll instead of re-transmitting. On iTerm2 this set stays empty and
every placement uses the full retransmit path.

### Negative-load TTL

`negative_loads` records the wall-clock when an image first failed to
load. Failures are sticky for `NEGATIVE_CACHE_TTL = 30 s` so a missing
or unreadable figure doesn't re-spawn `pdftoppm` on every scroll
keystroke. After the TTL the negative entry is dropped and the next
visibility tick retries — recovers if a still-downloading asset
finishes or a permissions issue is fixed.

### Off the hot path: a worker thread

PDF→PNG conversion via `pdftoppm` can take 50–200 ms for a complex
figure. Doing that on the reader thread would freeze the UI mid-scroll.
`ImageState` owns a worker thread:

- Reader schedules an `ImageJob { kitty_id, path }` and continues.
- Worker reads the file, runs `pdftoppm` if needed, normalizes the PNG,
  returns `ImageResult { kitty_id, png_bytes: Result<Vec<u8>, String> }`.
- Next frame, `poll_ready` drains the result channel and either fills
  `bytes` or records a negative load.

While bytes are pending the previous frame's placement is kept stable
— no flicker, no blank gap.

### Chunked transmission via `BatchEmitter`

`kitty_graphics::transmit::BatchEmitter` writes the Kitty `a=T` escape
in `m=1` continuation chunks and a single `m=0` terminator. The chunk
size is `kitty_graphics::transmit_byte_cap()` which adapts to the host:

- Native Kitty (not in tmux): generous cap (tested up to ~1 MB).
- iTerm2: conservative cap matched to its single-APC tolerance.
- Inside tmux: clamped to the passthrough size limit.

Capability detection (`kitty-graphics::detect`) chooses the cap based
on `TERM`, `TERM_PROGRAM`, `KITTY_WINDOW_ID`, and whether `TMUX` is
set. The env-var overrides
(`TREAD_DISABLE_KITTY_GRAPHICS`, `TREAD_FORCE_KITTY_GRAPHICS`) let
users opt out (e.g. a remote session over an iSH/SSH link that
mangles APCs) or in (inside tmux when env hints don't survive the
multiplexer).

### Burst skip

During fast scroll (`j` held), the user can't see individual frames
anyway, and re-emitting hundreds of KB per frame degrades scroll
latency. `BurstTracker::note_event` records each key event timestamp;
`after_draw_guarded` consults `in_burst()` (default 100 ms window) and
skips emission when the user is actively scrolling. The first frame
after a burst settles re-emits everything from scratch.

### Inline vs preview invalidation

Inline and preview image placements have separate caches inside
`ImageState`. A resize that affects only the preview pane invalidates
only the preview side; toggling preview off doesn't clear inline-image
state. Public `clear_images` clears both — used on resize, focus loss,
and clean shutdown — but internal paths use the split methods.

## Consequences

**Good:**
- The render pass remains a pure character-cell write. Visual drift
  bugs that aren't pixel placement can be debugged without reasoning
  about the escape stream.
- Idle CPU is dominated by the ratatui draw (~300 µs); idle bytes-to-
  terminal are ~0 because of `last_emitted`.
- Fast scroll stays responsive on image-heavy papers because of the
  burst gate.
- The image subsystem can be swapped (Sixel, iTerm2 inline, plain text
  fallback) by replacing the post-draw injector — the rest of the
  reader doesn't care.

**Costs:**
- The render and image paths are coupled by an unwritten contract:
  Image VLs occupy exactly `cols × rows` cells, ratatui must blank
  those cells, the injector must place at the same `(row, col)`. The
  contract is checked at construction time but not enforced by types.
- iTerm2 retransmits cost ~400 KB per visible figure per frame change.
  Acceptable today; would need a different strategy if we ever wanted
  60 fps animations.
- `images.rs` is a monolith today (worker, byte cache, PNG normalize,
  protocol emission, placement, trace logging). The `ImageState`
  external interface is the right seam, but the internal split is on
  the deepening backlog. Splitting is a refactor, not a redesign.
- Tmux without `allow-passthrough on` silently falls back to text
  placeholders (or, in the worst case, the "AAAA walls" failure mode
  described in the old `AUDIT.md` Z-item where the base64 payload
  appears as text). Tread prints a startup hint, but the failure is
  silent if the hint is ignored.

## Environment overrides

| Var | Effect |
|---|---|
| `TREAD_DISABLE_KITTY_GRAPHICS` | Force text fallback even on graphics-capable terminals. |
| `TREAD_FORCE_KITTY_GRAPHICS` | Override capability detection inside tmux when env hints don't survive. |
| `TREAD_TRACE_IMAGES` | Stderr trace of placements per frame (debug-only). |

## Validation

- `crates/tread/` acceptance tests cover preview/inline id ownership,
  burst-skip behaviour, and resize invalidation.
- The headless tmux smoke test in `crates/tread/ARCHITECTURE_PLAN.md`
  covers search, TOC, command jump, resize, and cleanup.
- Real-terminal validation lives outside CI: iTerm2, Kitty, WezTerm,
  Ghostty, and tmux-on-iTerm2 with passthrough on/off all need
  occasional manual sweeps when image-touching code changes.

## Open follow-up

- ~~Split private implementation modules behind `ImageState`~~ Done
  2026-05 (commit `6332f7f`; `inline.rs` / `preview.rs` / `worker.rs`
  / `png.rs`).
- The retransmit path on iTerm2 is the single largest source of bytes-
  to-terminal. Worth investigating whether iTerm2's native inline-image
  protocol (`OSC 1337 ; File=...`) has lower overhead than its Kitty
  emulation for our payload sizes. **Deferred** — requires benchmark
  setup on iTerm2 with representative figure-heavy papers.
- Capability detection currently runs once at startup. A `:reload` or
  detached/reattached tmux session can change the effective capability
  set; re-detect on focus regained would close that gap. **Deferred**
  — implementing this adds a query escape on every focus event, which
  has its own UX cost (terminal flicker / extra round-trip). Worth
  doing only with a real tmux-detach scenario to validate against.
