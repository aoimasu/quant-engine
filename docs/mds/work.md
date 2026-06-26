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

---

## QE-007 — Shared domain types — PR #7 — [Reviewed]

- **Branch:** `qe-007/shared-domain-types`
- **PR:** https://github.com/aoimasu/quant-engine/pull/7
- **Latest commit:** (see `git rev-parse HEAD` on branch / PR head)
- **Evidence/design:** `docs/architecture/qe-007-shared-domain-types-design.md`
- **Changed surface:** fills the `crates/domain` scaffold — `src/{lib,money,instrument,time,
  resolution,bar,funding,side,vintage}.rs`, `Cargo.toml`; root `Cargo.toml` (+`rust_decimal`
  workspace dep, +`proptest` dev workspace dep). Also bundles the QE-006 archive
  (`docs/mds/reviewed/qe-006.md`) — branch protection blocks direct `main` pushes.

### Acceptance criteria (copied from backlog)
- [x] Money arithmetic is exact (property tests for associativity/rounding policy).
      _(Arithmetic is exact and the proptests are genuine/non-vacuous — see review. NB the
      construction-time validation is bypassable at the serde boundary; tracked as Feedback #1, a
      separate soundness concern from arithmetic exactness.)_
- [x] Bar/resolution types are shared by both pipelines (single definition).
      _(`Resolution`/`Bar` are single definitions re-exported from `qe-domain`. But the `Bar` OHLC
      invariant is NOT enforced — public fields + serde bypass `Bar::new`; Feedback #1.)_

### Verification (re-run locally — all green)
- `cargo fmt --all --check` — ok
- `cargo clippy --workspace --all-targets --locked -- -D warnings` — clean
- `cargo test --workspace --locked` — `qe-domain` 29 tests pass (incl. 4 proptest laws); workspace green
- `cargo deny check` — advisories/bans/licenses/sources ok (proptest pulls a 2nd `rand` major →
  `multiple-versions = "warn"`, non-fatal)

Key AC-proving tests:
- **AC #1 (exact money)** — `money.rs` proptests: `notional_addition_is_associative` /
  `_is_commutative` / `notional_sub_inverts_add` (exact equality), and
  `rounding_stays_within_one_ulp_and_target_scale` (every `RoundingPolicy`: `scale() ≤ target` and
  `|rounded − exact| < 10^-scale`); unit tests for negative rejection, banker-vs-half-up midpoint,
  exact-string serde round-trip.
- **AC #2 (single bar/resolution definition)** — `Resolution` defined once in `qe-domain`,
  `FromStr`/`Display`/`minutes` round-trip tests; `Bar::new` OHLC-invariant validation tests. Both
  pipelines consume the one re-exported definition.

### Design notes for the reviewer
- Money is `rust_decimal::Decimal` (96-bit fixed-point, no binary float); the only rounding point is
  `Price::notional(qty, scale, policy)`. Decimals serialise as strings for exact JSON round-trips.
- Wiring `qe-config`'s string resolutions onto `Resolution` is intentionally deferred to QE-012 (its
  scope) to avoid touching an already-merged crate here.
- `qe-domain` keeps zero internal-crate deps, so the QE-001 topology guard is unaffected (re-run green).

### Review notes

**Verdict: [Reviewed]** — strong, well-tested crate with genuinely exact decimal money and a clean
module layout; the literal AC text is satisfied. Holding short of approval for **one systemic,
demonstrated soundness defect** (the validating constructors are bypassed at the serde/public-field
boundary), plus two minor edge notes. The defect is squarely in scope for "the shared, validated
vocabulary," so it should be fixed before this becomes the foundation everything else builds on.

**Independent re-verification (branch `qe-007/shared-domain-types`):**
- `cargo fmt --all --check` clean · `cargo clippy --workspace --all-targets --locked -- -D warnings`
  clean · `cargo test --workspace --locked` **80 passed, 1 ignored** (qe-domain 29) · `cargo deny
  check` ok (the 2nd `rand` major from `proptest` is `multiple-versions = "warn"`, non-fatal as
  documented) · QE-001 topology guard green (qe-domain has zero internal deps).

**What I verified positively (the adversarial focus areas):**
- **AC #1 money is exact, tests non-vacuous.** Confirmed the generators (`mantissa` in `0..1e9`,
  `scale 0..=8`) keep `p*q` at ≤18 significant digits / scale ≤16 — genuinely exact within Decimal's
  28-digit range, so `exact = p*q` is a *true* product and the ulp bound is **not** vacuously
  satisfied by a pre-rounded value. The associativity/commutativity/sub-inverse proptests use exact
  Decimal value-equality and hold over the domain; `rounding_stays_within_one_ulp_and_target_scale`
  is correct for **all four** policies (`Down`/`Up` directed rounding stays `< 1 ulp`; the two
  half-policies `≤ 0.5 ulp`). Decimal **string** serde round-trips exactly (verified). No negative-zero
  hazard — `rust_decimal` has no signed zero.
- **AC #2 single definition.** `Resolution` (one enum, `FromStr`/`Display`/`minutes` round-trip,
  derived `Ord` matches ascending duration) and `Bar` are defined once and re-exported. Half-open
  `[start,end)` interval semantics are correct; `Side`/`Direction` conversions are total and
  involutive. (Transient note: `qe-config` still carries its own string resolution ladder until
  QE-012 wires it onto `Resolution` — acceptable, documented, but "single across the codebase" isn't
  fully realized yet.)

### Feedback

1. **[Blocker — validating constructors are bypassed at the (de)serialization & field boundary].**
   Every validated type derives `#[derive(Deserialize)]` directly, so deserialization never runs
   `new`/validation; and `Bar`'s fields are all `pub`, so an invalid bar can also be built with a
   struct literal. I demonstrated this with a probe (deserializing crafted JSON):
   - `Price` ← `"-5.0"` → `Ok(-5.0)` (a **negative price**; `Price::new` rejects it). Same for `Qty`.
   - `InstrumentId` ← `"btc-usdt"` → `Ok("btc-usdt")` — **un-canonicalised** (lowercase + hyphen), so
     it won't `==` the canonical `BTCUSDT`, silently breaking the "same instrument → same id" contract
     that Eq/Hash-keyed lookups depend on.
   - `VintageHash` ← `"xyz"` → `Ok` (not 64-hex) — corrupts the audit key the firewall relies on.
   - `TimeInterval` ← `{"start":100,"end":50}` → `Ok` (reversed; `new` rejects it).
   - `Bar` ← `{... "high":"90","low":"95","close":"999" ...}` → `Ok`, with `range() = -5` (a negative
     range, violating the OHLC invariant the doc says `new` guarantees).
   These types are explicitly meant to be (de)serialized (the design says they feed lineage/hashing;
   bars come from storage in QE-010/011), so corrupt/malformed storage/config/feed data silently
   becomes invariant-violating domain values — exactly what a validated vocabulary exists to prevent,
   and the doc-comments actively claim ("validated", "Construction validates the OHLC invariant", "a
   non-negative price"). **Fix:** validate on deserialize — e.g. `#[serde(try_from = "Decimal")]` /
   `try_from = "String"` with `TryFrom` impls that call `new` (and a wire struct for `Bar` that calls
   `Bar::new`); make `Bar`'s fields private with getters (or `#[non_exhaustive]` + constructor-only).
   Add deserialize-**rejection** tests (the current serde tests only round-trip *valid* values, so
   they don't catch this).

2. **[Minor — "only rounding point" is slightly overstated].** `Price::notional` computes `self.0 *
   qty.0` then rounds once — correct for in-range inputs. But `rust_decimal`'s `*` itself rounds
   (banker's) when the true product exceeds 28 significant digits, and **panics** on 96-bit magnitude
   overflow. So for extreme-precision/huge price×qty there is a hidden second rounding (or a panic)
   before the explicit `round_dp`. Realistic crypto precision (≤8 dp) is safe and the proptest
   correctly stays in range, but either document the precision/magnitude precondition or use
   `checked_mul` and surface saturation.

3. **[Minor — overflow on `Notional` `+`/`-` panics, untested].** `Add`/`Sub` use `self.0 + rhs.0`,
   which panics on 96-bit overflow; `checked_add`/`checked_sub` exist but their `None` path has no
   test. Worth a test pinning the overflow contract, and note the panic-on-overflow tension if these
   are ever used in a QE-004 hot-path module (the clippy `panic` lint won't catch an arithmetic
   overflow panic).
