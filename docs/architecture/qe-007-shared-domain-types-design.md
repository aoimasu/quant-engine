# QE-007 — Shared domain types — design / evidence

## Ticket

`Phase: P0` · `Area: domain` · `Depends on: QE-001`

**Goal.** One vocabulary for instruments, time, bars, money, and direction prevents divergence
between training and runtime and underpins batch/streaming parity.

**Scope / requirements.**
- Types for: instrument id, venue, bar resolution, OHLCVT bar, funding rate sample,
  timestamp/interval (UTC, explicit precision), price/qty/notional (fixed-point decimal, no float
  money), side/direction, vintage hash.
- Conversions are total and tested; no silent precision loss on money.

**Out of scope.** Indicator/feature types (QE-107/108); strategy genome (QE-110).

**Acceptance criteria.**
- Money arithmetic is exact (property tests for associativity/rounding policy).
- Bar/resolution types are shared by both pipelines (single definition).

## Current-state evidence

- `crates/domain` is a QE-001 scaffold (`crate_name()` placeholder only, no deps). This ticket fills
  it in. It is the shared crate both pipelines may depend on, so the QE-001 topology guard treats
  `qe-domain` as neutral — keeping its dependency set tiny matters.
- `qe-config` already uses **string** resolutions (`"5m"`, `"30m"`, `"4h"`) and instrument strings,
  with an explicit comment that QE-007/QE-012 will reintroduce `qe-domain` newtypes. This ticket
  defines the canonical types; **wiring `qe-config` onto them is deferred to QE-012** (its own scope)
  to avoid touching an already-merged crate here.

## Design decisions

Fill `qe-domain` with small, total, well-tested modules. No binary floats anywhere.

### Money — `money.rs` (the heart of AC #1)
- `Price`, `Qty`, `Notional` wrap [`rust_decimal::Decimal`] — a 96-bit **fixed-point decimal**, so
  add/sub are exact and associative (no float error). `Price`/`Qty` are validated non-negative on
  construction (`new -> Result`); `Notional` is signed (it models exposure **and** realised PnL).
- The **only** rounding point is `Price::notional(qty, scale, policy) -> Notional`. Rounding is
  explicit: `RoundingPolicy { HalfEven (default, banker's), HalfUp, Down, Up }` maps to
  `rust_decimal::RoundingStrategy`. There is no implicit/hidden rounding elsewhere.
- Decimals serialise **as strings** (`#[serde(with = "rust_decimal::serde::str")]`) so JSON
  round-trips are exact — important because these types later feed lineage/content hashing (QE-006).

### Other modules
- `instrument.rs` — `InstrumentId` (canonical-uppercase, ASCII-alphanumeric, non-empty, validated)
  and `Venue` enum (`BinanceUsdtPerp`, the spec's venue).
- `time.rs` — `Timestamp` (i64 **milliseconds** since the Unix epoch, UTC, explicit precision; `Ord`)
  and `TimeInterval { start, end }` (half-open, `new` rejects `end < start`, `contains`/`duration`).
- `resolution.rs` — `Resolution` enum (`M1 M5 M15 M30 H1 H4 H12 D1`) with total `FromStr`/`Display`
  and `minutes()`. **Single definition** shared by both pipelines (AC #2).
- `bar.rs` — `Bar` (OHLCVT): `open_time`, `resolution`, OHLC as `Price`, `volume: Qty`, `trades:
  u64`. `new` validates `low ≤ {open,close} ≤ high`. (T = trade count, matching Binance klines.)
- `funding.rs` — `FundingRateSample { instrument, time, rate: FundingRate }`; `FundingRate` is a
  signed decimal newtype (funding can be negative).
- `side.rs` — `Side { Buy, Sell }` and `Direction { Long, Short }` with total, tested conversions
  (`Side::direction`, `Direction::side`, `opposite`).
- `vintage.rs` — `VintageHash`, a newtype validating a 64-char lowercase-hex digest (the shape of
  `Config::content_hash`/`Lineage::id`) — one type for the firewall's audit key.
- `lib.rs` — `DomainError` (thiserror) with one variant per validation failure; re-exports.

### Dependencies
`qe-domain` stays lean: `serde`, `thiserror`, `rust_decimal` (features `serde`, `serde-with-str`).
Dev: `serde_json` (round-trip tests), `proptest` (property tests). No internal-crate deps → the
topology guard is unaffected. New third-party crates (`rust_decimal`, `proptest` + transitives) are
all MIT/Apache-2.0; `proptest` may introduce a second `rand`/`rand_chacha` major — `deny.toml` has
`multiple-versions = "warn"` so that is non-fatal.

## Test plan (proves both ACs)

- **AC #1 (exact money):** `proptest` in `money.rs` —
  `notional_addition_is_associative`, `_is_commutative`, `notional_sub_is_add_inverse` (all **exact**
  equality), and `rounding_stays_within_one_ulp_and_scale` (for every policy, the rounded notional
  has `scale() ≤ target` and `|rounded − exact| < 10^-scale`). Plus unit tests: `Price::new` rejects
  negatives; banker's-vs-half-up differ on a `.5` midpoint; exact serde string round-trip.
- **AC #2 (single bar/resolution definition):** `Resolution` `FromStr`/`Display`/`minutes` round-trip
  tests; `Bar::new` validation tests (rejects `high < low`, `close > high`); the type is defined once
  in `qe-domain` and re-exported — both pipelines consume the same definition.
- Other modules: `InstrumentId` canonicalisation/rejection, `Timestamp`/`TimeInterval` ordering &
  `contains`, `Side`/`Direction` conversion totality, `VintageHash` accept/reject.

Gates: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`,
`cargo test --workspace --locked`, `cargo deny check`.

## Risks

- **`rust_decimal` rounding semantics:** mitigated by making rounding explicit (policy + scale) and
  property-testing the ulp/scale bound across all policies.
- **Decimal serde precision:** mitigated by string serialisation + an exact round-trip test.
- **Dependency growth from `proptest`:** dev-only; duplicate `rand` majors are `warn` in `deny.toml`.
- **Bar's "T" interpretation:** chosen as trade count (Binance kline field) and documented; if a
  later ingest ticket needs turnover too, it extends `Bar` additively.
