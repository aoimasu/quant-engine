# QE-448 — Correct the SPA label & document the biases deflation cannot see

`Phase: Review R2 (P3 — panel #19, unanimous)` · `Area: validation` · `Depends on: QE-131`
Spec of record: [`docs/reviews/2026-07-16-maxdama-panel-review.md#qe-448`](../reviews/2026-07-16-maxdama-panel-review.md).
Backlog: [Review R2.b](../backlog.md).

## 1. What `spa.rs` actually computes (evidence)

`crates/validation/src/spa.rs::reality_check_pvalue` builds the data-snooping p-value for the best
of `k` strategies:

- Observed statistic `V = maxₖ √T · sₖ · d̄ₖ`, where `d̄ₖ` is strategy `k`'s mean excess-over-benchmark
  and `sₖ = 1` (raw) or `1/σₖ` (studentised, `cfg.studentize`).
- Stationary-bootstrap (Politis–Romano) resamples give the null: for **every** `k`,
  `V*ᵦ = maxₖ √T · sₖ · (d̄*ₖ − d̄ₖ)` — i.e. **all k models are recentred by their own full-sample
  mean** `d̄ₖ` (`spa.rs:82–87`: the loop runs `for k in 0..n` with `resampled_mean - means[k]`, no
  per-model gate).
- `p = #{ V*ᵦ ≥ V } / B`.

**This is White's Reality Check (WRC, White 2000)** when means are raw (`studentize=false`, the
default), and **Hansen's "SPA-lower" bound** when studentised (`studentize=true`). Recentring *every*
model by its full mean is exactly the operation Hansen (2005) identified as the source of WRC's power
loss.

## 2. The White-RC-vs-Hansen distinction (why the old label was wrong)

Hansen's (2005) defining contribution is **not** studentisation — it is **model-omission recentring**.
He splits the k models by a data-dependent threshold `A_T = √(2 log log T)`:

- **SPA-lower** (`spa.rs` today): recenter **all** models by `d̄ₖ`. Poor models (large negative `d̄ₖ`)
  still contribute their bootstrap fluctuation to the null max, inflating the null and **raising the
  p-value → conservative, under-powered**.
- **SPA-consistent** (Hansen): recenter model `k` by `d̄ₖ` only if
  `d̄ₖ ≥ −(σₖ/√T)·A_T` — i.e. **drop the models too far below zero to be relevant**, so bad strategies
  stop polluting the null. This **recovers power** without breaking the size (still a valid test).
- **SPA-upper**: recenter nothing (`max(d̄ₖ, 0)`), the anti-conservative bound.

The module previously advertised **"White's Reality Check / Hansen's SPA"** (module doc, `lib.rs`,
`spa.rs`), and tagged `studentize` as *"Hansen's SPA refinement"*. That conflates studentisation with
Hansen's actual contribution and **claims the consistent test while implementing SPA-lower**. The
`sqrt(2 log log T)` threshold appears nowhere in the file — so the "Hansen's SPA" claim is false.

**Chosen fix: option (a) — relabel (golden-safe).** The spec leads with relabel as the honest,
low-risk fix, and the P3 debate note says "purely a power/clarity fix; safe direction". We relabel the
module accurately as **White's Reality Check / SPA-lower**, document that it is the conservative /
under-powered variant that omits Hansen's model-omission recentring, and note **option (b)** —
implementing SPA-consistent (`A_T` threshold) — as a future power upgrade.

**Why not (b) now.** SPA-consistent would move the computed p-value (the null distribution shrinks →
p drops). The SPA p-value rides the run-protocol sidecar / `RobustnessReport`, not `content_hash`, but
`crates/cli/tests/train_job.rs` and the gate fixtures assert specific `spa_pvalue` behaviour, and a
moved p-value can flip G1 promotion in a fixture. That is a power change with test churn, out of
proportion to a P3 clarity ticket. Relabel is the correct first step; (b) is a clean follow-up.

**No rename needed.** The public fn is **already** `reality_check_pvalue` — an accurate WRC name — and
`SpaConfig` names the test family. Renaming would churn `gate`, `cli`, `report`, `run-protocol` for no
semantic gain. The fix is **doc-comment accuracy + a semantics test**, so no public API and **no
golden** moves.

### Semantics test (proves the corrected label)

We add `all_models_recentred_is_conservative`: taking a genuinely-strong strategy among noise, then
**adding a high-variance zero-mean decoy**, the p-value **does not fall** (in practice rises) — the
signature of recentring *every* model (SPA-lower): an irrelevant noisy model inflates the null max and
costs power. Under Hansen's SPA-consistent that decoy would be thresholded out and could not inflate
the null. This pins the behaviour the label now claims.

## 3. The biases deflation cannot see (crate-doc boundary)

DSR / PSR / PBO / SPA all correct for **SELECTION** — the multiple-testing / best-of-N inflation from
searching a large quality-diversity archive. They are computed **on the return series they are given**
and say nothing about whether that series is itself honest. They **cannot** remove per-trade optimistic
bias:

- **Transaction-cost bias** — under-charged slippage/impact inflates every trade uniformly. DSR is
  *absolute* (vs a noise ceiling), so a systematic cost error flows through **undeflated**. Handled
  upstream by net-of-cost truth (QE-403) and cost calibration (QE-431/QE-440).
- **Adverse-selection bias** — maker-fill markout; a rebate that loses to adverse selection looks like
  free edge (QE-449).
- **Survivorship bias** — backtesting on a today's-membership universe that silently drops delisted
  blow-ups. Handled upstream by the point-in-time universe (QE-012) — see §4.

This boundary is now stated in the `qe-validation` crate doc so no reader mistakes a clean DSR/SPA for
proof the *inputs* were honest.

## 4. Universe provenance in the vintage lineage (finding: already captured)

**Question (spec):** is the QE-012 point-in-time / survivorship-safe universe membership captured in
the vintage lineage SHA — if yes, document it; if no, add it.

**Finding: already captured — no lineage change, no golden move.**

- `Lineage` (`crates/determinism/src/lineage.rs`) binds `config_hash`, `input_snapshot_id`,
  `code_commit`, `seeds`; `Lineage::id()` is SHA-256 over the canonical JSON of all four.
- The real path (`crates/cli/src/lib.rs:116`) builds it via
  `Lineage::from_config(cfg, "", code_commit, vec![seed])`, which folds in `Config::content_hash()`.
- `Config::content_hash()` (`crates/config/src/lib.rs`) is `Sha256(serde_json::to_vec(self))` over the
  **whole** `Config`, whose fields **include** `instruments` (flat roster) **and** `universe`
  (`Vec<UniverseMemberConfig>` — instrument + `listed`/`delisted` ISO dates, `schema.rs:159–188`).

Therefore the exact point-in-time roster — every instrument **and** its `[listed, delisted)`
survivorship window — is inside `config_hash`, hence inside the lineage id. Changing the roster, a
listing date, or a delisting date changes the vintage's resolvable id. A vintage is already traceable
to a specific survivorship-safe universe.

**Action:** document this in `lineage.rs` (so the guarantee is explicit, not incidental) and add a
regression test — two configs differing only in a `[[universe]]` delisting date must produce different
lineage ids. Documentation + test only; **no hashed field added → `VINTAGE_FORMAT_VERSION` unchanged,
no golden move.**

## 5. Scope of change (diff map)

| File | Change | Golden? |
|------|--------|---------|
| `crates/validation/src/spa.rs` | Relabel module/type/fn docs → White's RC / SPA-lower; document omitted Hansen recentring + follow-up (b); add semantics test | No |
| `crates/validation/src/lib.rs` | Fix module-doc SPA label; add deflation-boundary paragraph | No |
| `crates/determinism/src/lineage.rs` | Document universe provenance via `config_hash`; add regression test | No |
| `docs/architecture/qe-448-*.md` | This evidence note | No |

`qe-validation` still depends only on `qe-determinism`; `qe-determinism` already depends on
`qe-config` (via `from_config`) — **no new edge**. No public item renamed.

## 6. Green gate (local, CI disabled)

`cargo fmt --all -- --check`; `cargo clippy --workspace --all-targets --locked -- -D warnings` and
`--all-features --locked -- -D warnings`; `cargo test --workspace --locked`; `cargo deny check`;
`cargo test -p qe-architecture --test firewall`.
