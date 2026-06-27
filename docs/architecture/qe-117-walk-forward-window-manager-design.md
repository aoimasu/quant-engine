# QE-117 — Walk-forward window manager — design note

`Phase: P1` · `Area: ⑤ WFO` · `Depends on: QE-113`
`Branch: qe-117/walk-forward-window-manager`

## Goal (from backlog)

Rolling train/validate windows with purge+embargo are the backbone of WFO and continuous adaptation
without catastrophic forgetting.

- Generate anchored/rolling train→validate windows; apply purge gap and embargo per QE-113.
- Carry the archive across window transitions (persistence, not reset).

**Acceptance criteria.**
- [ ] For every window, train and test bar sets are disjoint **including lookback**.
- [ ] The archive persists across transitions; degraded strategies are displaced, not forgotten wholesale.

**Out of scope.** Archive internals (QE-118) — the manager threads a caller-owned archive; it does not
define one.

## Current-state evidence

- **QE-113** (`qe_wfo::cv`) fixed the leakage-free invariant: train/test information windows are disjoint
  iff `|tr − te| > lookback + label_horizon`, via a `purge = lookback + label_horizon` gap plus an
  embargo (`Fold::windows_disjoint`). This ticket applies the *same* invariant to **sequential** WF
  windows (train then validate-ahead), not k-fold.

## Design

### D1 — Window geometry (anchored + rolling)

A `WalkForward` config generates successive `Window { index, train, validate }` over `0..n_bars`:

- **train end** `te = origin + train_len` (exclusive); **origin** advances by `step` each window.
- **train range**: `Rolling` ⇒ `[origin, te)` (fixed-width, slides); `Anchored` ⇒ `[0, te)` (grows from
  the start — all history, no forgetting of distant regimes).
- **purge + embargo gap**: `validate` starts at `te + purge + embargo` with
  `purge = lookback + label_horizon` (QE-113). The gap sits **between** train and validate so neither's
  information window crosses into the other.
- **validate range**: `[te + purge + embargo, te + purge + embargo + validate_len)`.
- Generation stops when `validate_end > n_bars` (no partial validate window).

### D2 — Leakage-free by construction (AC1)

With the gap `= purge + embargo (≥ purge = L + H)`, for every train bar `tr ≤ te−1` and validate bar
`val ≥ te + purge + embargo`: `val − tr ≥ purge + embargo + 1 > L + H`. So
`Window::windows_disjoint(lookback, label_horizon)` (the same predicate as `cv::Fold`) holds for **every**
window — train and validate are disjoint *including the lookback*. The test asserts it over all windows
and both modes, and a contrast shows a zero-gap (naive) split would leak.

### D3 — Archive carried across transitions (AC2)

`run<A>(n_bars, &mut archive, on_window)` iterates the windows and invokes `on_window(&mut archive, &w)`
for each — threading the **same** `archive` instance through every transition. The manager **never
resets** the archive between windows; it is the caller's persistent state (QE-118 owns its internals).
The per-window callback may **displace degraded** entries (re-evaluate and drop those that fall below a
floor) while **retaining** entries not touched this window — adaptation without catastrophic forgetting.
The test uses a toy archive (strategy id → best fitness) and shows, across two windows: a re-evaluated
degraded strategy is displaced, an improved one updates, and an untouched strategy persists — not a
wholesale reset.

## Module / API plan

New module `crates/wfo/src/walkforward.rs`, re-exported:

- `WindowMode { Rolling, Anchored }`.
- `WalkForward { mode, train, validate, step, lookback, label_horizon, embargo }`; `purge()`.
- `Window { index, train: Range<usize>, validate: Range<usize> }`; `Window::windows_disjoint(lookback, label_horizon)`.
- `WalkForward::windows(n_bars) -> Vec<Window>`; `WalkForward::run<A, F: FnMut(&mut A, &Window)>(n_bars, &mut A, F)`.
- No new dependencies (pure index math; reuses the QE-113 invariant shape).

## Test plan (TDD)

1. **Rolling geometry.** Window count, train/validate ranges, `step` advance, gap `= purge + embargo`.
2. **Anchored geometry.** `train.start == 0` always; train grows each window; validate still gapped.
3. **Disjointness incl. lookback (AC1).** `windows_disjoint` true for every window in both modes; a
   zero-gap split is shown to leak (contrast).
4. **Archive carry (AC2).** `run` threads one archive; degraded displaced, improved updated, untouched
   retained across two windows (no wholesale reset).
5. **Edge.** `validate_end > n_bars` stops cleanly; `step` floored to ≥ 1; too-few bars ⇒ no windows.

## Gates

`cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`, `cargo test -p qe-wfo`,
`cargo test --workspace`.

## Risks

- **Embargo placement.** Embargo is folded into the train→validate gap (conservative); a separate
  validate→next-train embargo can be added when the archive-update path (QE-118/120) needs it. Documented.
- **Window sizes are pre-data constants.** `train`/`validate`/`step` are config-driven (QE-002) once real
  history lengths are known; the manager is count-agnostic.
- **Anchored growth cost.** Anchored train grows unbounded; acceptable for the P1 dev universe, revisit if
  evaluation cost dominates (a max-train cap is a trivial later addition).
