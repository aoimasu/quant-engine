# QE-466 — Vintage leaderboard / comparison surface (informational, NOT a selector)

> Evidence note (skill step 1). Written before implementation. Ticket:
> [`docs/mds/tickets/QE-466.md`](../mds/tickets/QE-466.md). Design ref:
> [`qe-455-research-flow-design.md`](../architecture/qe-455-research-flow-design.md) §9 (leaderboard /
> comparison), §3 (outer best-of-N REJECTED), §4 (single consultation), §11.1 (dominant risk).

## 1. Goal (restated)

A **read-only** vintage leaderboard / comparison that ranks **already-sealed** vintages on **persisted,
tradable, deflation-honest** metrics — all read from QE-467's sealed evidence, never recomputed — and is
**structurally incapable of selecting / promoting** (no outer best-of-N). This is the highest overfitting-risk
surface in the whole program; the point is that it CANNOT become an outer selector.

## 2. Current-state evidence (real file:line)

### QE-456 detail + QE-257 list endpoints (what we consume / mirror)
- `crates/server/src/read.rs:31` — `routes()` mounts `/vintages` (list) + `/vintages/{id}` (detail) +
  `/market-data/coverage`, all under the QE-256 `require_session` gate.
- `crates/server/src/read.rs:80` `list_vintages` — loads **full** `Vec<Vintage>` via
  `VintageRepository::list()` (each artefact hash-verified on load), projects to `VintageListItem`. The
  handler already has every vintage's full sealed `content` in hand (incl. `holdout_series.returns`).
- `crates/server/src/read.rs:206` `get_vintage` / `:225` `build_detail` / `:269` `build_detail_body` — the
  QE-456 detail read: reslices `content.seal_evidence`, `content.holdout_series` (handle + len),
  `content.provenance.{holdout_split,regime_composition,consultation_count,steer_delta}`, sidecars,
  producing-run reverse-join. **Pure read-over-sealed-artefact; recomputes nothing.** This is the template
  the leaderboard entry mirrors.

### QE-467 persisted sealed evidence (what we rank on — read, never recomputed)
- `crates/vintage/src/lib.rs:62` `SealEvidence` — `dsr`, `pbo`, `spa_pvalue`, `n_trials`,
  `realised_turnover`, `capacity_usd`, `cost_stress_net_min: Option<f64>` (**the deployed, capacity-capped,
  net-of-cost `min{1×,2×}` figure — the ranking key**), `uncensored_pbo`, `ic`, `fdr`.
- `crates/vintage/src/lib.rs:101` `HoldoutReturnSeries { returns: Vec<f64> }` + `:113` `handle()` — the
  **persisted net-of-cost holdout return series on the DEPLOYED capacity-capped weights** (QE-438). The exact
  series the cross-vintage correlation consumes. `content.holdout_series.returns`.
- `crates/vintage/src/lib.rs:171` `SteerDelta { indicator_subset_hash, generations, population, windows,
  folds }` — the per-vintage steer/param diff.
- `crates/vintage/src/lib.rs:237` `ResearchProvenance { data_provenance, holdout_split, regime_composition,
  consultation_count, steer_delta }`.

### QE-430 R(N) / Fisher-z correlation deflation (REUSED, not reimplemented)
- `crates/ensemble/src/objective.rs:123` `min_significant_r(n, z) = tanh(z/√(N−3))` (Dama's minimum-significant
  sample correlation).
- `crates/ensemble/src/objective.rs:165` `pairwise_corr_penalty(series: &[Vec<f64>], mode: CorrDeflation) ->
  CorrPenalty { value, effective_n }` — deflated positive-mean pairwise correlation + the **effective N** it
  rested on (smallest sample any pair used). Re-exported at crate root: `qe_ensemble::{pairwise_corr_penalty,
  CorrDeflation, CorrPenalty}` (`crates/ensemble/src/lib.rs:31`). **This is the exact QE-430 code the
  leaderboard calls — no reimplementation.**
- Caveat: `pearson` (`objective.rs:52`) returns `0.0` when the two series differ in length. Holdout series
  can differ in length across vintages, so the leaderboard **truncates each persisted series to the
  displayed-set minimum length** (leading bars, deterministic) before calling `pairwise_corr_penalty`, so the
  Pearson is over aligned equal-length slices and `effective_n` is that common length. The persisted series
  carry no per-bar timestamps, so bar-exact time alignment is a documented v1 limitation.

### QE-460 overlap-keyed consultation count (what we ENFORCE)
- `crates/cli/src/jobs/train.rs:290` — `consultation_count = 1 + count_overlapping_consultations(...)`.
  **Semantics:** `1` = this run is the FIRST/only consultation of its holdout (the honest single consultation
  design §4 mandates); `> 1` = the same holdout was already consulted by a prior overlapping run
  (multiple-testing at the campaign level). Persisted at `ResearchProvenance.consultation_count`.

### QE-457 rendering to reuse (frontend)
- `web/src/app/strategies/VintageInspector.tsx` exports `NotPaperConfirmedCallout` (`:171`),
  `ProvenanceBanner` (`:96`), `RegimeComposition` (`:185`) — reusable. The leaderboard reuses
  `NotPaperConfirmedCallout` for the per-surface "not paper-confirmed" framing, and the `DataTable` / `Card`
  / `Callout` / `Badge` design primitives (`web/src/design`).
- `web/src/app/strategies/VintageBrowser.tsx` — the list-table pattern the leaderboard table mirrors.
- `web/src/app/strategies/StrategiesArea.tsx:23` — a router-less view-state machine (`list | inspect`); the
  leaderboard is added as a third `leaderboard` view with a header toggle.

### SPA routing / API client
- `web/src/api/runs.ts:639` `listVintages()` / `:644` `getVintage()` over `getJson<T>` (`:553`) — the client
  pattern `getLeaderboard()` mirrors.
- `web/src/app/App.tsx` — `strategies` destination renders `<StrategiesArea/>`. No App change needed (the
  leaderboard is a sub-view inside Strategies).

### Firewall / dependency topology
- `crates/architecture/src/lib.rs:234` — the `qe-server` firewall rule forbids `qe-runtime`, `qe-venue`,
  `qe-runtime-core`, `qe-hedger`, `qe-edge`. **`qe-ensemble` is NOT forbidden** for `qe-server`, so adding a
  `qe-server → qe-ensemble` edge (to reuse QE-430's correlation code) keeps the firewall green. `qe-ensemble`
  itself reaches none of the qe-server-forbidden crates (its own rule forbids qe-wfo/runtime/venue/…), so no
  transitive breach. Verified against `crates/architecture/tests/firewall.rs`.

## 3. Implementation decisions

### 3.1 Endpoint (backend)
- `GET /api/vintages/leaderboard` — read-only, session-gated, added to `read.rs::routes()`. axum 0.8 / matchit
  routes the static `leaderboard` segment ahead of `/vintages/{id}`. Loads all sealed vintages via
  `VintageRepository::list()` (already hash-verified), reslices the persisted metrics, computes the
  cross-vintage correlation, enforces the consultation budget. GET-only: no POST/PUT/DELETE mounted.
- Response `Leaderboard { entries, cross_vintage_correlation, effective_n, effective_n_note,
  enforcement_posture, consultation_budget, not_paper_confirmed, caveat }`. Each `LeaderboardEntry` carries
  the persisted ranking metrics + `rank` ordinal + `over_consulted` + `dsr_status` + `steer_delta` + a
  per-entry `not_paper_confirmed: true`. **No promote/select/seal/winner field or action anywhere.**

### 3.2 Ranking metric — persisted net-of-cost only
Rank key = `cost_stress_net_min` (the DEPLOYED capacity-capped, net-of-cost `min{1×,2×}` figure from
`SealEvidence`, QE-467/438), descending; tie-break `dsr` desc then `id` asc for determinism. The DTO exposes
**no** gross Sharpe, equal-weight, lone-Sharpe, or in-sample metric — those fields do not exist on the
response, so they are structurally ABSENT (not merely hidden). `capacity_usd` and `realised_turnover`
(QE-467) are shown alongside.

### 3.3 Consultation-budget enforcement posture — **(b)**, and WHY
**Posture (b): rank ONLY on each vintage's own already-deflated sealed evidence, with NO fresh cross-vintage
selection statistic computed on holdout verdicts.** The cross-vintage correlation (QE-430 R(N)/Fisher-z) is
surfaced as a **diversity DIAGNOSTIC** — the effective N answers "are these vintages diverse, or the same bet
re-drawn?" — and is **never fed into the rank**.

Why (b) over (a) max-statistic/SPA-across-the-set: (a) would itself be a *fresh selection statistic over
holdout verdicts* — exactly the surface the design §3/§11.1 warns can regrow into the outer best-of-N
selector. (b) is the leaner, structurally-safe default the design names (§9): the leaderboard can confer **no
new deflation credit** — each vintage carries only the credit its own honest per-run G1 gate already sealed.
Ranking is a presentation ordering over independent, already-deflated numbers, not a test.

**Enforcement is structural, not cosmetic.** `HOLDOUT_CONSULTATION_BUDGET = 1` (conservative — design §4: the
backtest IS the single consultation; `count == 1` is the honest norm, `> 1` means the same holdout was
re-consulted). For an over-consulted vintage (`consultation_count > budget`):
1. `over_consulted = true` (surfaced),
2. `dsr_status = "escalated"` (the frontend greys-out / escalates the DSR bar),
3. **demoted below every within-budget vintage in the ranking**, regardless of its (possibly holdout-shopped)
   net-of-cost number.
So "re-run until the top slot improves" is defeated at the source: re-consulting the holdout increments the
count past budget, which **demotes** the vintage rather than promoting it — the leaderboard cannot be used to
shop the holdout. This is the "escalate the DSR bar so the top slot cannot be improved by re-runs" of §9,
made a hard ranking rule.

> **PRODUCT FLAG:** the over-consultation threshold is undefined in code. I pick **budget = 1** (most
> conservative, matches "single consultation" design §4). Returned to the orchestrator for product
> confirmation; nothing else depends on the exact value.

### 3.4 Structurally NOT a selector
- No promote / select-best / auto-run / seal endpoint or field — GET-only view over sealed artefacts.
  Promotion to a runtime vintage stays through the EXISTING per-run G1 gate + seal; the ranking confers no
  additional blessing.
- Every entry `not_paper_confirmed: true`; a top-level standing `caveat` states cross-vintage ranking is
  inspection and that re-running until the top slot improves is the rejected best-of-N pattern.
- No schema / `PROTOCOL_VERSION` / `VINTAGE_FORMAT_VERSION` bump (read-only surface). No gate/seal change.

## 4. Test plan (per AC)

Backend (`crates/server/tests/read.rs` acceptance + `read.rs` `#[cfg(test)]` unit):
- **AC1 ranks on persisted net-of-cost + shows steer diffs; gross/equal-weight/lone-Sharpe/in-sample absent** —
  `leaderboard_ranks_on_persisted_net_of_cost`: two vintages, higher `cost_stress_net_min` ranks first;
  assert `steer_delta` present; assert the JSON has no `gross_sharpe`/`equal_weight`/`sharpe`/`in_sample` key.
- **AC1 cross-vintage correlation + effective N** — `leaderboard_surfaces_cross_vintage_correlation_effective_n`:
  `cross_vintage_correlation` + `effective_n` present; `effective_n` = min series length.
- **AC2 consultation budget ENFORCED (chosen posture tested)** —
  `leaderboard_enforces_consultation_budget_demotes_and_escalates`: an over-consulted vintage (count 2) with a
  *higher* net-of-cost is `over_consulted=true`, `dsr_status="escalated"`, and ranked BELOW a within-budget
  vintage — proving enforcement, not display; plus `enforcement_posture == "own-evidence-only"` asserted.
- **AC3 no promote/select/seal/auto-run — read-only** — `leaderboard_is_read_only_rejects_mutating_verbs`
  (POST → 405) and `leaderboard_exposes_no_promote_or_select_action` (no promote/select/seal/winner key; every
  entry `not_paper_confirmed`).
- **AC4 standing caveat** — `leaderboard_carries_standing_best_of_n_caveat`.
- Session gate — `leaderboard_requires_session` (no session → 401).
- Pure-fn unit tests on `build_leaderboard` for ranking/enforcement/correlation without a running server.

Frontend (`web/src/app/strategies/VintageLeaderboard.test.tsx`):
- renders ranked rows with net-of-cost lead + capacity + turnover + effective N;
- over-consulted row greyed/escalated (`aria`/class + escalated DSR);
- steer/param diffs shown; `NotPaperConfirmedCallout` present; standing best-of-N caveat present;
- **no** promote/select/run button anywhere in the surface (assert absent);
- `StrategiesArea` toggles into the leaderboard view.

## 5. Risks
- **Route overlap** `/vintages/leaderboard` vs `/vintages/{id}` — matchit 0.8 prioritises the static segment;
  covered by an acceptance test that hits both.
- **New `qe-server → qe-ensemble` edge** — permitted by the firewall rule; firewall test re-run to confirm.
- **Series length mismatch** — handled by truncation to the common min length; documented limitation (no
  timestamp alignment in the persisted series).
- **Mis-reading enforcement as a selector** — mitigated by posture (b): correlation is diagnostic-only, rank
  is own-evidence-only, and the read-only/no-action tests lock the surface down.
