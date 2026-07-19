# Work — PR review tracker

Transient scratchpad for the **PR currently under review** only. On merge the approved block is archived to
`docs/mds/reviewed/<ticket>.md` and this file is cleared back to empty.

> **CI + branch protection are TEMPORARILY DISABLED for this run (user directive):** the green gate is the
> LOCAL full build/test/clippy/deny, and PRs are squash-merged without GitHub checks.

---

## PR #173 — QE-460 RunSpec::Flow composite run-kind + frozen-holdout carve (protocol 2→3) [Reviewed]

- **Ticket:** QE-460 · **Branch:** `qe-460/runspec-flow` · **PR:** https://github.com/aoimasu/quant-engine/pull/173
- **Latest commit:** `f8297a5`
- **Base:** `main`
- **Evidence note:** [`docs/operations/qe-460-composite-flow-analysis.md`](../operations/qe-460-composite-flow-analysis.md)
- **Ticket detail:** [`docs/mds/tickets/QE-460.md`](tickets/QE-460.md)
- **Design ref:** `docs/architecture/qe-455-research-flow-design.md` §5, §4

### Green-gate (all on commit `f8297a5`)
- `cargo fmt --all --check` — PASS · `clippy -D warnings` — PASS · `cargo test --workspace` — PASS (91 suites, 0 failed)
- firewall — PASS · `cargo deny check` — PASS · backend-only (no web/)
- PROTOCOL_VERSION==3 (assertion + agreement-test bytes updated); NO `VINTAGE_FORMAT_VERSION` bump (stays 8, only populates QE-467's `ResearchProvenance`); NO `evaluate_g1`/search/DE/seal change; QE-006 determinism green (3 passed); un-flow vintages byte-identical.

### AC → proving test
1. atomic train→backtest, one status, content-hash handoff, failed-G1 seals nothing/no backtest: `flow_sequences_train_then_backtest_and_records_sub_runs`, `flow_with_failed_g1_train_runs_no_backtest_and_fails` (`supervise_flow` in manager.rs).
2. backtest is the single recorded holdout consultation (gate verdict re-surfaced, no independent deflation credit): `flow_seal_records_frozen_holdout_split_and_regime_composition` asserts backtest window == recorded holdout range.
3. regime-stratified holdout (≥K labels), floored/embargoed, edge server-derived, split+regimes in lineage: `flow_seal_records_frozen_holdout_split_and_regime_composition`, `holdout_regime_composition_collapses_to_one_regime_on_a_constant_series`/`..._spans_multiple_regimes...`, `validate_flow_rejects_gate_decision_knobs_and_sub_floor_holdout_embargo`.
4. consultation counter overlap-keyed (intersection not equality): `flow_consultation_counter_is_overlap_keyed_not_equality`, `consultation_overlap_is_keyed_on_intersection_not_equality`.
5. train+backtest pin same sealed cost calibration (parity): `flow_backtest_params_pin_the_sealed_cost_model_and_pass_parity`, `flow_cost_parity_rejects_a_friendlier_friction_model`.
6. re-run from seed+snapshot byte-identical vintage: `flow_seal_is_byte_identical_for_the_same_seed_in_a_fresh_repo`.
7. PROTOCOL_VERSION→3; no 2nd VINTAGE_FORMAT_VERSION bump; firewall green: `protocol_version_is_three` + firewall suite.
8. validate_flow blocklist/floors: `validate_flow_*` suite (reuses `validate_train` via `FlowParams::to_train_params()`).

### Design notes for the reviewer to scrutinise
- **Flow-level atomicity (not seal-level).** A real `qe train` seals its vintage regardless of the G1 verdict (seal is out of scope), so `supervise_flow` treats a non-promoted G1 or unsealed vintage as a FLOW failure and runs no backtest. Confirm this genuinely prevents a G1-reject flow from surfacing a backtest number.
- **Firewall-safe handoff.** The train→backtest handoff (frozen holdout window + config-derived instrument) is read from the train sub-run's `result.json` as an OPAQUE JSON value, so the server gains NO `qe-cli` edge. Verify no new cross-crate edge and firewall stays green.
- **QE-458 follow-up CLOSED for the flow path** — holdout-geometry assertion keyed on named QE-125 regimes (≥K labels) AND OOS span in bars (`HOLDOUT_FLOOR`), which is exactly QE-458's deferred AC(d). Plain-`train`'s separate deferral untouched (out of scope).

### FLAGGED defaults (repo-undefined — conservative picks, reviewer/product to confirm)
- `K` (min holdout regime labels) = 2 (`MIN_HOLDOUT_REGIMES`, new const; 4 regimes exist).
- `N` (min folds) ← `MIN_WFO_FOLDS`=2; holdout floor ← `HOLDOUT_FLOOR`=250; embargo floor ← `EMBARGO_FLOOR`=1 (reused compiled floors).

### Acceptance criteria (from `docs/mds/tickets/QE-460.md`)
- [ ] `type:"flow"` create → one supervised run sequencing train→backtest atomically w/ content-hash vintage handoff; failed-G1 train seals nothing & runs no backtest; one status row.
- [ ] Backtest is the single recorded holdout consultation, no independent deflation credit; no "backtest disjoint from holdout" claim remains.
- [ ] Holdout regime-stratified / multi-fold WFO (≥K labels or N non-contiguous embargoed folds), floored (`400` below floor / zero embargo), edge server-derived if single trailing block; split + regime composition in `VintageContent.lineage`.
- [ ] Consultation counter increments overlap-keyed (holdout intersection, not exact equality).
- [ ] Train + backtest sub-runs pin the same sealed cost calibration (asserted equal).
- [ ] Re-run from recorded seed + pinned snapshot reproduces the vintage byte-identically.
- [ ] `PROTOCOL_VERSION` bumped to 3 (assertion updated); no 2nd `VINTAGE_FORMAT_VERSION` bump; firewall/dependency-topology green (no new cross-crate edge).

### Review verdict — Reviewed (reviewer, commit `f8297a5`)

**Most of this large ticket is excellent and correctly done — but there is ONE blocking defect in the integrity heart of the ticket (AC 5 cost parity), which also taints AC 2.** The composite lifecycle, atomicity, overlap-keyed consultation counter, regime-stratified carve, determinism, protocol bump, and firewall are all correctly implemented and well-tested (I re-ran the flow suites, firewall, protocol, and determinism — all green on `f8297a5`). The blocking item is subtle and exactly the class of deflation-contract error this review had to catch.

**BLOCKING (1):**
1. **AC 5 cost-parity is against the WRONG baseline — the flow re-costs the holdout 2.5× cheaper than the gate (a friendlier friction — the precise breach AC 5 / maxdama #6 forbids).** `crates/server/src/runs/manager.rs:722` pins `const FLOW_TAKER_FEE_BPS: f64 = 2.0` (→ 2 bps taker) for the holdout backtest. But the train gate priced BOTH selection and the **G1 holdout evaluation** at `train_cfg.friction = BacktestConfig::default().friction` → `FeeSchedule::default().taker = Decimal::new(5, 4) = 0.0005 = 5 bps` (`crates/wfo/src/friction.rs:57-63`, used at `crates/cli/src/jobs/train.rs:597-607`; the holdout returns feeding G1 are `combine(..., &train_cfg)`). So the flow's single holdout consultation is re-costed at **2 bps vs the gate's 5 bps — 2.5× friendlier**. Consequences: (a) AC 5 is not met — the backtest re-costs the holdout under a friendlier friction than the gate used; (b) AC 2 is tainted — the flow-backtest number is NOT the gate's holdout verdict re-surfaced, it is a rosier number computed under cheaper fees. The `flow_cost_parity_ok` guard and its `flow_cost_parity_rejects_a_friendlier_friction_model` test give **false assurance**: they assert parity against `FLOW_TAKER_FEE_BPS = 2.0` (the standalone-CLI `BacktestParams` default), not the 5 bps the gate actually used, so a genuinely friendlier fee passes the guard. The `FLOW_TAKER_FEE_BPS` doc-comment's claim that 2.0 "equals `BacktestConfig::default().friction`" is **false** (that default's taker is 5 bps, not 2). *(Slippage/impact parity IS correct — content-addressed from the sealed vintage's `SlippageCalibration`; only the taker fee is mismatched.)*
   **Minimal fix:** set `FLOW_TAKER_FEE_BPS = 5.0` (= `FeeSchedule::default().taker × 10_000`); better, derive the flow backtest's fee from the same `FeeSchedule::default()` the gate uses (single source of truth) rather than a literal so it can never drift again; update the parity test + the `_rejects_a_friendlier_friction_model` case to assert against 5 bps; and correct the doc-comment. Add a test that the flow backtest's effective taker fee equals the gate's `train_cfg.friction` fee.

**Ruling on the flagged defaults (K=2 `MIN_HOLDOUT_REGIMES`, N=2 folds, holdout 250, embargo 1): ACCEPTABLE as flagged placeholders — non-blocking.** All are enforced (K<2 → hard `RunError::HoldoutRegimeCoverage`; holdout/embargo/windows/folds floors → `400` via the reused `validate_train`) and documented in-code with a "FLAGGED for product confirmation" note. K=2 is conservative (4 regimes exist); N/holdout/embargo reuse *existing* compiled floors (`MIN_WFO_FOLDS`/`HOLDOUT_FLOOR`/`EMBARGO_FLOOR`), not newly invented values. This matches how `MIN_OCCUPIED_NICHES` was handled on QE-458 (flagged placeholder acceptable when enforced + documented). Product should confirm K=2 before it hardens, but it does not block merge.

**Verified-correct (no action needed):**
- **AC 1 atomic / failed-G1 no-backtest — CONFIRMED.** `supervise_flow` (`manager.rs:823`) treats `!sealed_ok || !promoted` (G1 not promoted, or no sealed vintage) as a flow FAILURE and runs no backtest. `flow_with_failed_g1_train_runs_no_backtest_and_fails` genuinely asserts the backtest sub-run id is null, its dir is never created, and the error names G1. One run-store row, content-hash vintage handoff.
- **AC 3 regime-stratified / floored / edge-server-derived / lineage — CONFIRMED.** `build_flow_lineage` (`train.rs:242`) hard-errors below `MIN_HOLDOUT_REGIMES`; the holdout window edge is server-derived (`flow_end` = pinned snapshot right edge); `{holdout_range, train_range, embargo}` + regime composition are written to `VintageContent.provenance`. A single trailing block asserting ≥K regimes is the AC-sanctioned v1 form.
- **AC 4 overlap-keyed counter — CONFIRMED.** `consultation_overlaps` is genuine half-open interval intersection (`ps < end && start < pe`) OR prior-train-covers; `count = 1 + priors`. The unit test asserts partial-overlap and containment count TRUE while touching/disjoint count FALSE — it would fail a naive equality impl — and the integration test seals two different-sized holdouts (60 vs 40 bars) into one repo and asserts `consultation_count == 2` with unequal ranges.
- **AC 6 determinism — CONFIRMED (with a note below).** `flow_seal_is_byte_identical_for_the_same_seed_in_a_fresh_repo` runs twice from seed 42 into separate fresh artifact roots → identical `content_hash`. Plain (non-flow) vintages record no holdout lineage → byte-identical to pre-QE-460 (golden-safe).
- **AC 7 protocol / no 2nd vintage bump / firewall — CONFIRMED.** `PROTOCOL_VERSION == 3` (assertion + `agreement.rs` golden bytes all use 3); no `crates/vintage` change (only populates QE-467's `ResearchProvenance`, format stays 8); the train→backtest handoff reads `result.json` as an opaque `serde_json::Value` — `qe-server` has NO `qe-cli` dependency (deps: telemetry/storage/vintage/signal/risk/formula-pool/validation/run-protocol/config; none forbidden), firewall test green. No stale "backtest disjoint from holdout" active claim survives (only explicit corrections/negations remain).

**Non-blocking notes:**
1. **Determinism reproducibility subtlety.** `consultation_count` is part of the content-hashed `ResearchProvenance`, so a flow vintage's `content_hash` depends on the campaign/repo state at seal time (how many prior overlapping holdouts exist), not seed + snapshot alone. The byte-identical AC is met for the tested clean-repo case, but reproducing a *campaign-position* vintage requires reproducing the campaign state. This is a QE-467 schema property (content-addressing a stateful counter), not a QE-460 defect — flagging for product awareness (consider whether the consultation count belongs in the hashed content or a sidecar).
2. **`holdout_window` is date-granular** (`format_ymd`) while the carve is bar-precise, so the re-surfaced backtest window may include a bar or two outside the exact holdout slice at the date boundary (implementer-documented v1 limitation). Combined with the blocking fee bug, the re-surfaced number is doubly-approximate; once the fee is fixed, consider a bar-precise handoff or note the residual.
3. Stale doc-comment at `crates/run-protocol/tests/agreement.rs:87` still says "protocol version (QE-452: 2)" though the golden bytes correctly use 3 — cosmetic.
