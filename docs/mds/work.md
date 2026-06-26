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
- **PR:** _(set on `gh pr create`)_
- **Latest commit:** _(see PR head)_
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

_(awaiting dedicated review agent — `start-review-ticket` against this branch/diff vs the ACs above)_
