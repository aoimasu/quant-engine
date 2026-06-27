# QE-206 — Factor join + batch/streaming parity tests — design note

`Phase: P2` · `Area: ④ / shared` · `Depends on: QE-205, QE-107` · `Branch: qe-206/factor-join-parity`

## Goal (from backlog)

Parity by construction is the whole point of the shared catalogue; this proves it end-to-end on the live
path.

- Live factor join producing factor rows via the shared catalogue; a parity test suite comparing batch vs
  streaming factor rows.

**Acceptance criteria.**
- [ ] Live factor rows equal offline feature vectors bar-for-bar on shared fixtures.

**Out of scope.** Evaluator session (QE-207).

## Current-state evidence & placement

- The shared catalogue + feature assembly already exist and are batch/streaming parity-proven in isolation:
  `qe_signal::feature::{FeatureAssembler (streaming push), assemble_batch, FeatureVector, FeatureSchema}`
  (QE-107/108). A `FeatureVector` is the per-bar **factor row**: quantised states for every catalogue
  indicator at one timestep.
- A `qe_signal::Sample` is **the join unit**: `{ bar, funding, open_interest, premium }` — the base bar
  plus the aligned scalar context the flow factors read. Offline, samples are built by joining the scalar
  context to each base bar; `assemble_batch(cfg, &samples)` yields the **offline feature vectors**.
- So the missing piece QE-206 supplies is the **live factor join**: on the live path, align the latest
  scalar context (funding/OI/premium) to each incoming base bar to form a `Sample`, then drive the shared
  `FeatureAssembler`. Placement: `qe-runtime` (already deps `qe-signal`; the live pipeline lives here next
  to `LiveKlineSource` from QE-205). No new deps; no firewall change.

## Design

### D1 — The as-of context join

Funding / open-interest / premium arrive on their own live streams, interleaved in time with bars.
`LiveFactorJoin` keeps the **last-known** value of each (a streaming as-of join): a context observation
updates the current snapshot; an incoming base bar is paired with the snapshot in force at that moment.
Because the runtime feeds events in timestamp order, the snapshot at a bar is exactly the most-recent
context value with `ts <= bar.open_time` — the same as-of rule an offline batch join applies. Context with
no observation yet is `None` (matching `Sample::from_bar`, and the flow factors' own warm-up handling).

### D2 — Drive the shared assembler (no second code path)

`on_bar(bar)` builds `Sample { bar, funding, open_interest, premium }` from the current snapshot and calls
the **unmodified** `FeatureAssembler::push` — the identical catalogue `update` path offline assembly uses.
So a live factor row is `assemble_batch`'s vector for the same sample, by construction. The only new logic
is the join (snapshotting context onto bars); reconstruction/quantisation/assembly are all reused.

### D3 — Parity proof (the deliverable)

The parity test compares two genuinely different join implementations over a shared fixture (base bars +
timestamped context observations):
- **offline**: a batch as-of join — for each bar, pick the latest context value with `ts <= bar.open_time`
  — builds `Vec<Sample>`, then `assemble_batch(cfg, &samples)` → offline vectors.
- **live**: interleave the bar and context events in timestamp order through `LiveFactorJoin` (context
  events call the setters, bars call `on_bar`) → live vectors.

Assert the two vector sequences are equal **bar-for-bar** (time + every quantised state). Both reach the
same `FeatureAssembler`, so any divergence can only come from a join bug — exactly what the test guards.

## Module / API plan

New module `crates/runtime/src/factor_join.rs`, re-exported from `lib.rs`:
- `LiveFactorJoin { assembler: FeatureAssembler, funding/open_interest/premium: Option<Decimal> }`.
- `new(cfg) -> Self`, `schema() -> FeatureSchema`.
- `observe_funding(Decimal)`, `observe_open_interest(Decimal)`, `observe_premium(Decimal)` — update the
  as-of snapshot (called in ts order as context events arrive).
- `on_bar(&Bar) -> FeatureVector` — join + drive the assembler.
- `reset()`.
- Plus an offline reference join helper for the test: `as_of_join(bars, funding_obs, oi_obs, premium_obs)
  -> Vec<Sample>` (a plain batch as-of), so the parity test's offline side is explicit and independent.

`rust_decimal` becomes a (non-dev) dependency of `qe-runtime` since the join handles `Decimal` context
values in non-test code (currently it is a dev-dep).

## Test plan (TDD)

1. **Live factor rows == offline vectors (AC).** Shared fixture: ~30 base 5m bars + funding/OI/premium
   observations at assorted timestamps (some between bars, some exactly on a bar, some before the first
   bar). Offline as-of join → `assemble_batch`; live interleaved through `LiveFactorJoin`. Assert equal
   bar-for-bar (and assert at least one vector is `is_complete()` so the comparison is non-trivial, and
   that the flow factors actually moved — context is genuinely consumed).
2. **As-of semantics.** A bar before any context observation → `None` context (flow factors not warm); a
   context update strictly between two bars applies to the later bar, not the earlier; a context obs with
   `ts == bar.open_time` applies to that bar.
3. **Bar-only parity.** With no context at all, live rows still equal `assemble_batch` of bar-only samples
   (the price factors path).
4. **Schema agreement.** `LiveFactorJoin::schema()` equals `FeatureSchema::from_catalogue(cfg)`.
5. **Reset.** `reset()` returns the assembler to pre-warm (a fresh run reproduces the same vectors).

## Gates

`cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`, `cargo test -p qe-runtime`,
`cargo test --workspace`, `cargo test -p qe-architecture --test firewall`, `cargo deny check`.

## Risks

- **Parity is the point** and holds because the live join reuses the unmodified `FeatureAssembler`; only
  the as-of snapshotting is new, and the test pits it against an independent batch as-of so a join bug
  cannot hide. The offline reference deliberately does **not** share code with the streaming snapshot.
- **As-of ordering** assumes context events are fed in ts order (the live-stream contract; QE-202 already
  reports gaps/ordering). Same-ts tie-break: context applied before the bar (so `ts == bar_time` counts),
  matching the offline `<=` rule — pinned by a test.
- **`rust_decimal` promoted to a normal dep** of `qe-runtime` (was dev-only). It is already a workspace
  dep used pervasively; `cargo deny` is unaffected.
- **Firewall.** No new crate edges; `qe-runtime` keeps `qe-signal`/`qe-venue`/…; QE-132 guard stays green.
```
