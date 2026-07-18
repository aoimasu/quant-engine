# QE-451 Phase 1b — GP-aware deflation + tradability gates + formula-pool freeze (default-off) — review record

*QE-451 epic, Phase 1b of 3 — **the phase that COMPLETES the QE-451 GP-engine epic** (Phases 0 → 1a → 1b).*

- **PR**: https://github.com/aoimasu/quant-engine/pull/154 (squash-merged)
- **Branch**: qe-451-p1b/deflation-gates-freeze
- **Implementation commit**: `093915f` (fix pass; round-1 [Approved] was `3e1ea15`)
- **Spec of record**: `docs/architecture/qe-450-gp-indicator-evolution-design.md` §5 (overfitting backbone), §4.6 (fitness + tradability gates), §8 (prerequisites), §9 (Phase 1b row), §12 (dissents)
- **Evidence note**: `docs/architecture/qe-451-phase1b-deflation-gates-freeze-design.md`
- **Prior phases (merged)**: [`qe-451-phase0.md`](./qe-451-phase0.md), [`qe-451-phase1a.md`](./qe-451-phase1a.md)

## Acceptance criteria (Phase 1b — completes QE-451) — all met
- [x] **GP-aware deflation basis**: `N = max(distinct-canonical count, cells·gens·windows)` → QE-439 `effective_trials`/`expected_max_sharpe_ln`; **uncensored** dispersion/PBO over EVERY evaluated formula; uncensored PBO is the PRIMARY GP gate, DSR necessary-not-sufficient. *(DSR-axis honesty requires the calibrated N* folded in — see carry-forward.)*
- [x] **IC pre-screen (QE-434)**: purged rank-IC two-fold + BH-FDR; every screened formula (pass AND fail) counts toward N.
- [x] **In-search MDL rent (QE-436)** in the pool-selection objective, kept OUT of the DSR-facing fitness; hard depth/node/lookback caps.
- [x] **Deflated + purged ensemble correlation penalty (QE-430)** for evolved members + leave-one-PROVENANCE-out floor + evolved-share cap.
- [x] **Tradability gates at archive insertion (§4.6)**: cost-stress min over m∈{1.0,2.0} (QE-431 calibration) finite & >0; max-turnover REJECT (avg hold ≥4h); capacity floor $250k (QE-440). Rejects STILL count toward N.
- [x] **Cross-asset pooled fitness**: shared formula scored across perps; effective-independent `T_eff` from MEASURED cross-asset return correlation.
- [x] **Label-shuffle / block-bootstrap null** CALIBRATED so shuffled-champion DSR ≈ 0.5 across node-size bands.
- [x] **Freeze K≤16** into `CatalogueIdentity`/vintage: `formula_hash` = canonical S-expr SHA-256; load boundary asserts exact identity (tamper → `SchemaMismatch`).
- [x] **Golden-safety**: DEFAULT (empty-pool) vintage BYTE-IDENTICAL — no golden moved; `CATALOGUE_VERSION`=1 / `VINTAGE_FORMAT_VERSION`=7 unchanged; all machinery opt-in.

## Implementation (per-control → merged prereq reused)
1. `wfo/gp/deflation.rs`: `N=max(distinct-canonical, cells·gens·windows)`; uncensored dispersion/PBO over the full evaluated population (`variance_trials = population.len()`, time-major CSCV, fails closed when unestimable); PBO primary, DSR necessary-not-sufficient — **QE-439** + QE-414.
2. `wfo/gp/gates.rs`: purged rank-IC two-fold + BH-FDR — **QE-434** `screen_catalogue`.
3. `deflation.rs`: in-search MDL rent kept out of the DSR-facing fitness — **QE-436** caps.
4. `ensemble/objective.rs`: deflated corr penalty + leave-one-PROVENANCE-out floor + evolved-share cap; evolved members cross as DATA (no firewall edge) — **QE-430** `pairwise_corr_penalty`.
5. `gates.rs`: cost-stress `min{1×,2×}` (reuses QE-431 `cost_sweep`/`from_calibration`) + max-turnover-reject (≥4h) + inlined capacity floor $250k reading the shared `SlippageCalibration` (coefficient-parity test; avoids the firewall edge) — **QE-431** + **QE-440**.
6. `deflation.rs::pooled_t_eff`: cross-asset `T_eff` from measured correlation — new.
7. `nulls.rs`: label-shuffle / block-bootstrap null — extends **QE-131**.
8. `wfo/gp/freeze.rs` + `signal/feature.rs`: freeze K≤16, `formula_hash` = canonical S-expr SHA-256 (reuses Phase-1a `canonical_hash`), load boundary rejects mismatch — **QE-402** + **QE-444** (empty-pool omitted via `skip_serializing_if`).

**Shuffle-null calibration (headline honesty test):** genuinely evolves a max-Sharpe champion over 1200 seeded (`task_rng`) label-shuffles per node band. Non-vacuous: step (1) raw basis N=P UNDER-deflates (champion DSR > 0.7 on pure noise); step (2) `calibrate_null_basis` yields N*≥P (conservative); step (3) shuffled-champion **DSR ∈ [0.45,0.55] ≈ 0.5 across all three node-size bands**. Test `shuffle_null_champion_dsr_is_near_half_across_node_bands`.

**Default-off / golden-safe:** `CatalogueIdentity.formula_pool` uses `#[serde(default, skip_serializing_if="Vec::is_empty")]` → empty pool omitted → default identity & `content_hash` unchanged; regenerate → empty diff (reviewer independently re-ran `regenerate_fixtures`).

## Review — two rounds + delta
**Round 1 (`3e1ea15`): [Approved], 0 blocking, 3 non-blocking.** Maximally-adversarial audit (deflation.rs read in full; gates/mod/freeze/nulls/objective independently audited):
- **Rejects ALL count toward N — CONFIRMED, no dodge.** In `illuminate()` the count is unconditional and *before* any gate/insert: `eval_tree → distinct.insert(hash); total += 1 → archive.insert`. Phase-1b gates are standalone downstream functions with no counting loop → no formula can architecturally dodge N.
- **Uncensored PBO genuinely primary + over the full evaluated population — CONFIRMED** (N-independent binding gate, fails closed when unestimable).
- **Shuffle-null calibration real, not rigged** (1200 seeded shuffles/band; the ≈0.5 landing is a convergence check on the calibrator, not a constructed pass).
- **Gates genuinely BLOCK** (hard `Reject` verdicts, three reject tests); cost-stress **reuses** QE-431 `cost_sweep`; capacity parity-tested against the shared `SlippageCalibration`.
- **Freeze golden-safe + exact** (`formula_hash = canonical_hash` reused, K≤16, empty pool omitted; reviewer independently re-ran regenerate → empty; legacy records deserialize).
- **Firewall clean** (wfo new deps = qe-validation + sha2, no qe-ensemble; evolved members cross as DATA; freeze crosses as sealed serde `Vec<String>`).
- Green gate on `3e1ea15`: fmt/clippy(both)/**957 passed, 2 ignored**/deny/firewall all green.

**Fix pass (`093915f`) delta re-review: [Approved] stands, 0 blocking.** All three round-1 findings resolved, genuine + non-vacuous, no golden moved:
1. **§4 wording** — qe-450 §4.6/§5/AC5 + evidence note now state the DSR-honest basis is `max(distinct, N*)` (N*≥P calibrated); raw `max(distinct, analytic-floor)` under-deflates DSR; stale `[0.35,0.65]` corrected to `[0.45,0.55]`; the **QE-454 carry-forward** ("production seal MUST fold N* in") is prominent + load-bearing.
2. **Tamper-load test** `a_tampered_formula_pool_is_rejected_at_the_load_boundary` — seals a tampered `formula_hash`, asserts `repo.load → SchemaMismatch{expected==current(), found==tampered}`; untampered empty-pool loads clean.
3a. **Single-dedup-reject test** `a_dedup_rejected_offspring_still_counts_toward_the_trial_basis` — two identical trees, second is a real `DedupRejected` → `total==2` / `distinct==1` / `archive==1`.
3b. **Zscore-affine canonical strip** — PURELY ADDITIVE (`Rank | Zscore` roots; `zscore(a·x+b)=zscore(x)` for a>0); `zscore(neg(x))` stays distinct. Critical additivity proof: `golden_mutation_stream_is_pinned` (4 pinned hashes) + Phase-0 seam byte-identity pass UNCHANGED → no exercised `formula_hash` moved.
- Delta green gate on `093915f`: fmt/clippy(both)/**960 passed, 2 ignored** (+3 new tests)/deny/firewall all green; regenerate empty; versions unchanged.

## Verification (LOCAL green gate re-run by reviewer on `093915f`, CI disabled — all PASS)
fmt · clippy locked + all-features `-D warnings` · `cargo test --workspace --locked` (**960 passed, 2 ignored**) · deny · firewall. No golden moved (regenerate → empty).

## Load-bearing carry-forward to QE-454 (production-seal governance)
The DSR axis is honest **only with the calibrated N* folded in**. The shipped `assess_gp_champion`/`gp_trial_basis` are basis-agnostic (take the count as a param) and use `max(distinct, analytic-floor)`, which UNDER-deflates the DSR axis (the PR's own shuffle-null step-1 shows DSR>0.7 on pure noise). Non-blocking HERE because (a) the uncensored-PBO primary gate is honest and non-parametric, (b) Phase-1b is default-off with NO production accept path, (c) the `calibrate_null_basis` tool + its necessity are delivered and demonstrated non-vacuously. **QE-454 MUST seal on `max(distinct, N*)`, not the analytic floor alone** — this is the single most important carry-forward for the production-seal ticket.

### Non-blocking (accepted; nothing outstanding blocks the epic)
- (Deferred to QE-452/Phase 2) co-evolution / regime-conditioned pooling.
- (Deferred to QE-454) the production seal governance: `DEFLATION_BASIS_VERSION` const, server routes, RBAC, tamper-evident audit, admin UI — AND the N* basis carry-forward above.

## Epic status — QE-451 COMPLETE
- Phase 0 (seam proof) — delivered ([`qe-451-phase0.md`](./qe-451-phase0.md)).
- Phase 1a (offline GP pool) — delivered ([`qe-451-phase1a.md`](./qe-451-phase1a.md)).
- Phase 1b (deflation + gates + freeze) — **delivered** (this record). **The QE-451 GP-engine epic is complete.**
