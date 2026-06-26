# Work — PR review tracker

Active PRs awaiting/under review for the P0/P1 ticket run. Each entry is reviewed by the
dedicated review agent, which writes `[Reviewed]`/`[Approved]` + comments inline. On merge, the
approved block is archived to `docs/mds/reviewed/<ticket>.md` and removed from here.

> **Branch protection note (since QE-005):** `main` requires CI checks (`fmt`/`clippy`/`test`/`deny`)
> with `enforce_admins=true`, which blocks direct pushes. Archive bookkeeping for a merged ticket is
> therefore committed on the *next* ticket's branch so it flows through a PR + CI.

## Completed (archived in `docs/mds/reviewed/`)
- QE-001 — Cargo workspace & crate topology — PR #1 — Approved & merged.
- QE-002 — Configuration system — PR #2 — Approved & merged.
- QE-003 — Structured logging & tracing — PR #3 — Approved & merged.
- QE-004 — Error model & result conventions — PR #4 — Approved & merged.
- QE-005 — CI pipeline — PR #5 — Approved & merged.
- QE-006 — Determinism & reproducibility harness — PR #6 — Approved & merged.
- QE-007 — Shared domain types — PR #7 — Approved & merged.
- QE-008 — Clock-skew / time-sync guard — PR #8 — Approved & merged.
- QE-009 — Risk-limit & kill-switch contract — PR #9 — Approved & merged.
- QE-010 — LMDB market-data store — PR #10 — Approved & merged.
- QE-011 — LMDB synthetic-data store — PR #11 — Approved & merged.

---

## QE-012 — Instrument-universe configuration & point-in-time membership — PR #12 — [Ready-for-review]

- **Branch:** `qe-012/instrument-universe`
- **PR:** https://github.com/aoimasu/quant-engine/pull/12
- **Latest commit:** _(post-approval advisory #1 follow-up — see below)_
- **Evidence/design:** `docs/architecture/qe-012-instrument-universe-design.md`
- **Changed surface:** `crates/config` — **new** `src/universe.rs` (`Universe`, `InstrumentListing`,
  ISO-date→`Timestamp` civil-date math), **new** `tests/universe.rs` (4 integration tests);
  `src/schema.rs` (+`UniverseMemberConfig`, +`Config.universe` field); `src/lib.rs`
  (+`Config::universe()` builder, universe validation wired into `validate`, exports); `Cargo.toml`
  (reintroduce `qe-domain` path dep). Also bundles the QE-011 archive
  (`docs/mds/reviewed/qe-011.md`) + `docs/mds/work.md` bookkeeping — branch protection blocks direct
  `main` pushes.

### Acceptance criteria (copied from backlog)
- [x] A backtest window excludes instruments not yet listed / already delisted at that time.
- [x] Changing the universe size requires only config, no code change.

### Verification (re-run locally — all green)
- `cargo fmt --all --check` — ok
- `cargo clippy --workspace --all-targets --locked -- -D warnings` — clean
- `cargo test --workspace --locked` — `qe-config` 22 unit (8 new universe) + 4 layering + 4 new
  universe integration; QE-001 topology guard green; workspace green
- `cargo deny check` — advisories/bans/licenses/sources ok (no new third-party deps; `qe-domain` is
  an internal path dep)

Key AC-proving tests:
- **AC #1 (point-in-time exclusion)** — `universe.rs::members_at_respects_listing_and_delisting`
  and `tests/universe.rs::point_in_time_membership_excludes_unlisted_and_delisted`: BTC listed
  `2019-09-08`, ETH listed `2019-11-27` delisted `2025-01-01`; `members_at` returns `[]` before any
  listing, `[BTC]` before ETH lists, `[BTC,ETH]` while both live, `[BTC]` after ETH delists.
  `membership_boundaries_are_half_open` pins `listed` inclusive / `delisted` exclusive.
  `delisted_symbols_stay_in_all_known` proves the delisted symbol is retained in the full roster (no
  survivorship drop).
- **AC #2 (config-only resize)** — `tests/universe.rs::universe_size_is_config_only` (1- vs
  3-instrument `[[universe]]` TOML through the identical code path) and
  `flat_instruments_fallback_is_open_ended` (date-less configs keep working as always-member);
  `universe.rs::universe_is_count_agnostic`.
- **Validation** — `invalid_universe_entry_is_rejected_at_load` (delisted < listed → fail-fast with
  the `universe[0].delisted` dotted path); date golden values + leap-year/day-overflow rejection in
  `days_from_civil_golden_values` / `parse_iso_date_rejects_malformed_and_out_of_range`.

### Design notes for the reviewer
- **Point-in-time membership** is a half-open window test mirroring `qe_domain::TimeInterval`:
  `listed <= as_of < delisted`. `delisted = None` = still trading; an open-ended listing uses a
  `Timestamp::MIN` sentinel (`OPEN_LISTING`) so it's a member at every instant.
- **Config-driven & count-agnostic:** `[[universe]]` (instrument + optional `listed`/`delisted` ISO
  dates) resolves via `Config::universe()`; when absent it **falls back** to the flat `instruments`
  list as open-ended listings, so existing date-less configs are unaffected. Same code serves 1 or N
  instruments.
- **No survivorship bias:** `Universe::all_known()` returns the full roster incl. delisted symbols;
  filtering to a point in time is an explicit `members_at` call — blown-up coins are never silently
  dropped at load.
- **Dates without a date crate:** ISO `YYYY-MM-DD` → UTC-midnight `Timestamp` via Howard Hinnant's
  `days_from_civil` + per-month/leap-year validation — exact and deterministic (no `chrono`); config
  still serialises from `Vec`/scalar only, so `content_hash` stays stable.
- **Validation** is folded into `Config::validate` (building the universe), so a bad id / date /
  ordering / duplicate fails fast at load with a dotted `universe[i].field` path.
- **Topology:** reintroduces the `qe-config → qe-domain` internal edge (anticipated by the crate's
  own `Cargo.toml` comment); neither pulls `wfo`/`ensemble`/`runtime`, so the QE-001 guard stays
  green (verified).

### Review notes

**Verdict: [Approved].** Reviewed strictly as architect + senior engineer against the full diff vs
`main` (head `cf9708d`). Both ACs are met — by construction and by test — date math is correct,
validation fails fast at load, and the topology edge is sound.

**AC #1 — point-in-time exclusion (PASS).** `InstrumentListing::is_tradable_at` implements the half-open
window exactly: `self.listed <= as_of && self.delisted.is_none_or(|d| as_of < d)` — `listed` inclusive,
`delisted` exclusive, `None` delisting = unbounded — mirroring `qe_domain::TimeInterval`.
`members_at`/`is_member_at` filter on it; `all_known()` returns the **full** roster so delisted symbols
are never silently dropped (survivorship-bias-free). Covered by `members_at_respects_listing_and_delisting`,
`membership_boundaries_are_half_open` (pins the inclusive/exclusive boundaries), `delisted_symbols_stay_in_all_known`,
and the integration test `point_in_time_membership_excludes_unlisted_and_delisted`.

**AC #2 — config-only resize (PASS).** `Config::universe()` builds from the `[[universe]]` TOML section
when present, else falls back to the flat `instruments` list as `open_ended` listings — one count-agnostic
code path for 1 or N instruments. Verified `universe_size_is_config_only` (1 vs 3 via identical path) and
`flat_instruments_fallback_is_open_ended` (date-less configs unchanged, always-member). Precedence is
correct: when `[[universe]]` is non-empty the flat list is ignored for membership but still validated.

**Date math (PASS).** ISO `YYYY-MM-DD` → UTC-midnight via Howard Hinnant `days_from_civil`, no external
crate. Verified the golden values **by hand**: 1970→2000 = 30×365 + 7 leap days (1972…1996) = 10957 days;
1969-12-31 = −1 day; epoch = 0. Per-month/leap-year rejection (`2021-02-29`, `2020-04-31`, `2020-13-01`,
width/separator) is exercised. Note the 4-digit year cap (width-10 check) bounds `days * 86_400_000` well
within `i64` — no overflow path. Deterministic, so `content_hash` stays stable (new field is `Vec`/scalar
with `#[serde(default)]`).

**Validation fail-fast (PASS).** `Config::load` → `validate()` → `self.universe()?`, so a bad id / malformed
date / `delisted < listed` / duplicate fails at load with a dotted `universe[i].field` path
(`invalid_universe_entry_is_rejected_at_load` asserts `universe[0].delisted`). The fallback path also runs
`InstrumentId::new` on the flat list (the comment's "non-empty + dup-checked above" is accurate — `validate`
checks those at lines 87–99 before calling `universe()`), tightening flat-list validation as intended.

**Topology (PASS).** Independently confirmed `qe-domain` is a true leaf (deps: serde/thiserror/rust_decimal,
**zero** internal `qe-` edges), so the reintroduced `qe-config → qe-domain` edge cannot transitively reach
`wfo`/`ensemble`/`runtime` — the QE-001 guard stays green regardless of where it runs.

**Verification caveat (transparency).** I could **not** independently re-run the cargo gates this pass: the
Rust toolchain is absent from this review environment (no `cargo`/`rustc`/`rustup`). The verdict rests on
full static review of all changed source, hand-verification of the date algorithm and golden values,
diff-level confirmation of the `load → validate → universe` wiring, and structural confirmation that
`qe-domain` is a leaf. I did not rely on the PR's "all green" claim as evidence; treat the reported gate
results as developer-reported. Nothing in the static review contradicts them.

**Advisories (non-blocking — do not gate merge):**
1. **Two ISO-date validators with different strictness.** `history.start` is still validated by the
   pre-existing `is_iso_date` (format + month 1..=12 + day 1..=31 only — **no** per-month/leap check), so
   `history.start = "2021-02-29"` or `"2020-04-31"` would pass there while the universe's new `parse_iso_date`
   correctly rejects them. Now that the calendar-strict parser exists, `history.start` could reuse it and the
   two paths consolidate. Partly pre-existing (QE-002); newly fixable here. Not a QE-012 AC defect.
2. **`validate()` builds the universe purely to validate and discards it**, and consumers rebuild via
   `universe()`. Cheap and idiomatic fail-fast, fine to leave — noted only for awareness (a `build_universe`
   helper returning the value could let `validate` reuse it if it ever caches the resolved config).

### Post-approval follow-up (coder) — advisory #1 resolved; status → [Ready-for-review]

Addressed non-blocking advisory #1 (date-validation consolidation). Strictly a correctness +
dedup improvement; no behaviour weakened.
- **#1 (consolidate `history.start` onto strict `parse_iso_date`) — DONE.** `Config::validate` now
  validates `history.start` through the same leap-year / per-month-aware `parse_iso_date` used by the
  universe, and the weaker duplicate `is_iso_date` helper is **removed**. `history.start` now rejects
  calendar-invalid dates the old check let through (e.g. `2021-02-29`). Replaced the helper's direct
  unit test with `history_start_uses_strict_calendar_validation` (rejects `2021-02-29`, accepts the
  real leap day `2020-02-29`) — one definition of "valid ISO date" across the config.
- **#2 (validate builds-and-discards the universe) — left as-is:** intentional fail-fast — building
  the universe *is* its validation; the cost is negligible and storing it would change `Config`'s
  shape for no functional gain.
- Gates re-run green: fmt ok; clippy clean; `qe-config` 22 unit + 4 layering + 4 universe
  integration; deny unaffected (no dep change).
