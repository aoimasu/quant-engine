# QE-012 — Instrument-universe configuration & point-in-time membership

`Phase: P0` · `Area: cross-cutting / data` · `Depends on: QE-002 (config), QE-007 (domain)`

## Goal

A **config-driven, instrument-count-agnostic** universe with **point-in-time membership** so a
backtest as-of a given instant only sees instruments that were tradable then — no survivorship bias.
Delisted / blown-up symbols are retained historically, never silently dropped.

## Current state (evidence)

- `qe-config` (QE-002) resolves a typed [`Config`](../../crates/config/src/schema.rs) from layered
  TOML + env, with a flat `instruments: Vec<String>` field (default `["BTCUSDT","ETHUSDT"]`) and
  field-level validation in `crates/config/src/lib.rs` (`validate`). It has **no** `qe-domain`
  dependency yet — the `Cargo.toml` comment explicitly reserves that for QE-007/QE-012: *"will
  reintroduce qe-domain once config references shared newtypes (instrument/resolution)."*
- `qe-domain` (QE-007) owns [`InstrumentId`](../../crates/domain/src/instrument.rs) (validated
  ASCII-alphanumeric newtype) and [`Timestamp`](../../crates/domain/src/time.rs) (epoch-millis UTC).
  There is **no `Date` type** — config dates are ISO `YYYY-MM-DD` strings, format-checked by a local
  `is_iso_date` helper but never converted to an instant.
- The flat `instruments` list has no listing/delisting dimension, so a point-in-time query is
  impossible today. That is exactly the gap QE-012 fills.

## Design

Add a `universe` module to **`qe-config`** (reintroducing the `qe-domain` dep) plus a `[universe]`
config section. The universe is built and validated from config; point-in-time membership is a pure
query over it.

### Types (`crates/config/src/universe.rs`)

- `Universe { listings: Vec<InstrumentListing> }` — the resolved, validated universe.
- `InstrumentListing { instrument: InstrumentId, listed: Timestamp, delisted: Option<Timestamp> }`
  — a half-open tradability window `[listed, delisted)`; `delisted = None` means still trading.
- Construction validates: instrument ids parse (`InstrumentId::new`), each `delisted >= listed`, and
  no duplicate instrument appears twice in the universe.

### Point-in-time query (the AC)

- `Universe::members_at(as_of: Timestamp) -> Vec<InstrumentId>` — the instruments whose window
  contains `as_of`: `listed <= as_of && as_of < delisted` (delisting **exclusive**, mirroring
  `TimeInterval`'s half-open convention). Returned in stable config order.
- `Universe::is_member_at(&InstrumentId, as_of) -> bool` — single-instrument predicate.
- `Universe::all_known() -> &[InstrumentListing]` — the **full** roster including delisted symbols,
  so callers that need the historical set (corpus loading) never silently drop blown-up coins.
- `Universe::len()` / `is_empty()` — count-agnostic (works for one instrument or many).

### Date handling

ISO `YYYY-MM-DD` → `Timestamp` at **UTC midnight** via Howard Hinnant's branch-free `days_from_civil`
algorithm (exact, no external date crate, fully deterministic). A bare date with no time-of-day maps
to `00:00:00Z`. Calendar-validates month/day ranges (rejects `2020-13-01`, `2020-02-30`-style day
overflow per-month including leap years).

### Config integration (`schema.rs` + `lib.rs`)

New optional section:

```toml
[[universe]]
instrument = "BTCUSDT"
listed     = "2019-09-08"      # delisted omitted → still trading

[[universe]]
instrument = "ETHUSDT"
listed     = "2019-11-27"
delisted   = "2025-01-01"      # excluded from windows on/after this date
```

- `Config.universe: Vec<UniverseMemberConfig>` (default empty), where
  `UniverseMemberConfig { instrument: String, listed: Option<String>, delisted: Option<String> }`.
- `Config::universe() -> Result<Universe, ConfigError>` builds the resolved `Universe`:
  - if the `[[universe]]` table is non-empty, from those entries;
  - else **backward-compatible fallback**: from the existing flat `instruments` list, each as an
    open-ended listing (`listed = Timestamp::MIN-ish`, `delisted = None`) so today's configs keep
    working and every listed instrument is always a member.
- `Config::validate` is extended to validate the `[[universe]]` section up-front (parseable ids,
  ISO dates, `delisted >= listed`, no duplicates) with dotted field paths
  (`universe[i].instrument`, `universe[i].listed`, …), preserving the crate's fail-fast style.

### Why this shape

- **Config-only resize (AC #2):** adding/removing instruments or changing windows is pure TOML; no
  code path is instrument-specific. The same code serves 1 or N instruments.
- **Point-in-time (AC #1):** `members_at` is a half-open window test — an instrument not yet listed
  (`as_of < listed`) or already delisted (`as_of >= delisted`) is excluded.
- **No survivorship bias / explicit delist policy:** delisted symbols stay in `all_known()`; they're
  only filtered by an explicit as-of query, never dropped at load.
- **Determinism:** dates resolve through a pure civil-date algorithm; the config still serialises
  from `Vec`/scalar fields only, so `content_hash` stays stable.

## Test plan (TDD)

`crates/config/src/universe.rs` unit tests + `crates/config/tests/` integration:

- **AC #1 — point-in-time exclusion:** a universe with BTC listed `2019-09-08` and ETH listed
  `2019-11-27`, ETH delisted `2025-01-01`:
  - `members_at(2019-10-01)` → `[BTCUSDT]` (ETH not yet listed);
  - `members_at(2020-01-01)` → `[BTCUSDT, ETHUSDT]`;
  - `members_at(2025-06-01)` → `[BTCUSDT]` (ETH delisted);
  - `members_at(2019-01-01)` → `[]` (nothing listed yet);
  - boundary: `members_at(listed)` includes it (inclusive); `members_at(delisted)` excludes it
    (exclusive).
- **AC #2 — config-only resize:** a 1-instrument and a 3-instrument `[[universe]]` TOML both load and
  query correctly through the identical code path; the flat-`instruments` fallback yields an
  always-member universe.
- **Validation:** rejects a bad instrument id, a malformed date, `delisted < listed`, and a duplicate
  instrument — each with the right dotted field path.
- **Date algorithm:** `days_from_civil` golden values (epoch `1970-01-01 → 0`, a known modern date),
  leap-year day-overflow rejection.
- **Determinism:** `content_hash` stable across loads of a universe config.

## Risks / out of scope

- **Out of scope:** per-instrument archive sharding (QE-118); real trading-calendar / intraday
  listing times (dates are UTC-midnight granular — sufficient for daily-boundary backtests).
- **Risk:** introducing the `qe-domain` dep to `qe-config` must not create a cycle — `qe-domain` has
  no dep on `qe-config`, and neither pulls `wfo`/`ensemble`, so the QE-001 topology guard stays green.
- Custom civil-date math is small but must be correct; covered by golden-value + leap-year tests.
