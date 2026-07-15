# QE-402 — Pin catalogue identity in the vintage & assert exact schema match on load

`Phase: PreP3` · `Area: trading / reproducibility` · `Priority: P1 correctness` · **Supersedes QE-262**,
folds in the schema-registry umbrella (AR-7).

Authoritative spec: `docs/reviews/2026-07-15-team-improvement-review.md` → `### QE-402`.

## Problem (current state, file:line)

- The vintage persists **neither** `CATALOGUE_VERSION` **nor** `states`. `VintageContent`
  (`crates/vintage/src/lib.rs:30-49`) carries `format_version`, `vintage_id`, `chromosomes`, `weights`,
  `calibration`, `worst_case_loss`, `lineage` — no catalogue identity.
- At load, the schema is **rebuilt from `CatalogueConfig::default()`**: `catalogue_config()` /
  `catalogue_schema()` (`crates/cli/src/jobs/features.rs:37-45`) construct `FeatureSchema` from the
  current build's default catalogue.
- `check_schema` (`crates/cli/src/jobs/features.rs:52-63`) only **bounds-checks** each clause via
  `Genome::is_valid` (feature index `< schema.len()`, state `< num_states`). It catches out-of-range
  drift, **not** identity drift.
- Consequence: a catalogue **reorder** (same width, same `num_states`) or a **same-width
  `CATALOGUE_VERSION` bump** is undetectable. A sealed genome's clause `feature = 7` silently addresses a
  **different indicator** at backtest/live time → wrong signals, wrong PnL, broken reproducibility.
- Architect finding AR-7: `VINTAGE_FORMAT_VERSION` (`crates/vintage/src/lib.rs:27`), `CATALOGUE_VERSION`
  (`crates/signal/src/indicator/mod.rs:28`), genome `REP_VERSION` (`crates/signal/src/genome.rs:27`) and
  the market store `SCHEMA_VERSION` (`crates/storage/src/lib.rs:24`) are versioned independently with **no
  compatibility assertion at the load seams**.

## Design

### 1. New persisted field — `CatalogueIdentity` (in `qe-signal`)

New serializable type `qe_signal::feature::CatalogueIdentity`
(`crates/signal/src/feature.rs`):

```
CatalogueIdentity { catalogue_version: u32, num_states: u16, id_hash: String }
```

- `id_hash` = lowercase-hex SHA-256 over the ordered indicator ids joined by `\n` — so any **reorder**,
  add, or rename of an indicator changes the hash.
- `CatalogueIdentity::from_schema(&FeatureSchema)` — the honest identity of a given schema.
- `CatalogueIdentity::current()` = `from_schema(FeatureSchema::from_catalogue(&CatalogueConfig::default()))`
  — the identity this build addresses genomes against (the same schema the whole pipeline uses).
- `CatalogueIdentity::hash_ids(&[String])` — exposed so tests can prove a reorder changes the hash.
- Adds `sha2` to `qe-signal`'s deps. `sha2 = "0.10"` is already a workspace dependency
  (`Cargo.toml:23`, used by `qe-vintage`/`qe-determinism`), so **no new external crate** enters the
  workspace and `cargo-deny` is unaffected.

`VintageContent` (`crates/vintage/src/lib.rs`) gains `pub catalogue: CatalogueIdentity`, placed before
`lineage`. `qe-vintage` already depends on `qe-signal` (it embeds `Genome`), so this pulls in no new edge
and keeps the firewall intact. **`VINTAGE_FORMAT_VERSION` bumped 2 → 3.**

### 2. Exact-match assertion, fail-closed, typed error

New artefact-schema module `qe_vintage::schema` (`crates/vintage/src/schema.rs`) provides
`assert_schema(&VintageContent) -> Result<(), VintageError>`:

- **vintage↔catalogue**: `content.catalogue != CatalogueIdentity::current()` ⇒
  `VintageError::SchemaMismatch { expected, found }` (loud, typed).
- **vintage↔genome rep**: any chromosome whose `version != qe_signal::REP_VERSION` ⇒
  `VintageError::GenomeRepMismatch { index, expected, found }`.

Wired at the **load boundary**: `VintageRepository::load` (`crates/vintage/src/lib.rs`) calls
`schema::assert_schema` after the content-hash verify. This is the single by-id load used by **both** the
CLI backtest (`crates/cli/src/jobs/backtest.rs:97`) and the live runtime
(`ActiveVintage::load` / `rollover_from`, `crates/runtime/src/vintage_rollover.rs:48,137`). So backtest
**and** live fail closed on a mismatched catalogue.

`VintageRepository::list()` and the low-level `Vintage::load(reader)` intentionally keep only the
hash-verify (list stays tolerant — a stray/foreign artefact is skipped, not fatal, per the existing
contract). The server read path lists, so it never hard-errors on an unrelated file.

### 3. Artefact-schema registry module (AR-7 umbrella)

`crates/vintage/src/schema.rs` documents, in one place, **every persisted artefact format version and the
load boundary that must assert it**:

| Artefact | Version constant | Load boundary asserting it |
|---|---|---|
| Vintage | `VINTAGE_FORMAT_VERSION` (=3) | `Vintage::load` content-hash verify |
| Catalogue (vintage↔catalogue) | `qe_signal::CATALOGUE_VERSION` + `id_hash` | `VintageRepository::load` → `schema::assert_schema` (**this ticket, real + enforced**) |
| Genome representation (vintage↔genome) | `qe_signal::REP_VERSION` | `VintageRepository::load` → `schema::assert_schema` |
| Market/synthetic store | `qe_storage::SCHEMA_VERSION` (=1) | `MarketStore::open` → `check_or_init_schema` (`crates/storage/src/engine.rs:45-63`, already asserts `StorageError::SchemaMismatch`) |

For the store row the module **centralizes the constant + a documented compatibility note** and points at
the existing, already-enforcing boundary. It deliberately does **not** take a `qe-storage` dependency:
`qe-vintage` is loaded by the live runtime, and linking LMDB/`heed` into it would be a footprint/firewall
regression. The vintage↔catalogue assertion is the one made **real and enforced** here; the store row is
incremental (constant + note) as the spec permits.

## Fixture regeneration (format genuinely changed ⇒ hashes change)

Adding `catalogue` to `VintageContent` changes its canonical JSON → its `content_hash` → the derived
vintage id. Regenerating the golden fixtures is **correct and required**. All hashes are recomputed by
round-tripping through the real seal/hash code — never hand-edited.

Fixtures/goldens carrying a vintage or a vintage-derived hash:

- `crates/cli/tests/fixtures/sample_vintage.json` — sealed vintage (embeds `content_hash`).
- `crates/cli/tests/fixtures/golden_result.json` — backtest golden, embeds
  `strategy.params.content_hash` + `format_version`.
- `crates/server/tests/fixtures/sample_vintage.json` — byte-identical copy of the CLI fixture.

**Reproducible procedure** (the only way the new hashes are produced):

1. Update `write_sample_vintage` in `crates/cli/tests/backtest_job.rs` to set
   `catalogue: CatalogueIdentity::current()` (the fixture genome addresses feature 0 of the default
   catalogue, so `current()` is its true identity).
2. Run the existing `#[ignore]`d regenerator, which seals via `Vintage::seal` and writes the golden via
   the real `run_backtest`:
   ```
   cargo test -p qe-cli --test backtest_job regenerate_fixtures -- --ignored --exact
   ```
   This rewrites `sample_vintage.json` (new `content_hash`, `format_version: 3`, `catalogue {…}`) and
   `golden_result.json` (new embedded hash + `format_version: "3"`).
3. Copy the regenerated `crates/cli/tests/fixtures/sample_vintage.json` to
   `crates/server/tests/fixtures/sample_vintage.json`.

No test embeds an OLD-format vintage against a NEW-format loader after this: the server read test asserts
only `content_hash.len()==64` and `format_version.is_u64()` (`crates/server/tests/read.rs:113-122`);
`train_job`/`backtest_job`/runtime tests all seal fresh through the current code path.

## Determinism preservation

- `CatalogueIdentity` is a pure function of the (deterministic) catalogue: fixed indicator order, fixed
  `CATALOGUE_VERSION`, fixed `num_states`, SHA-256 over the joined ids. Same build ⇒ same identity ⇒ same
  serialized bytes ⇒ same `content_hash`.
- The existing `train_is_deterministic_for_a_fixed_seed` (`crates/cli/tests/train_job.rs`) already asserts
  two runs at the same seed produce a byte-identical sealed content hash; it continues to hold under the
  new format (the added field is seed-independent and deterministic).
- `crates/determinism/tests/determinism.rs` pins stream/reduction goldens unrelated to the vintage hash —
  unaffected.

## New tests (exact-match proof)

In `crates/vintage/src/schema.rs`:
- A vintage sealed with `CatalogueIdentity::current()` loads (round-trips through `VintageRepository`).
- A vintage sealed with the same width/`num_states` but `catalogue_version + 1` is **rejected** on load
  with `SchemaMismatch` (proves same-width version-bump detection).
- Reordering two indicator ids yields a **different `id_hash`**, and a vintage carrying that identity is
  **rejected** on load (proves reorder detection).
- A vintage carrying a chromosome with a wrong `REP_VERSION` is rejected with `GenomeRepMismatch`.

## Risks / rollback

- **Risk:** every existing sealed vintage on disk is old-format and will fail the hash verify / catalogue
  assert on load. Migration is explicitly out of scope (spec). Acceptable: vintages are re-sealed by
  training; this is a pre-live reproducibility hardening.
- **Risk:** all 8 in-repo `VintageContent { … }` constructors must add the field (compile-enforced) —
  covered: `crates/vintage/src/lib.rs` (tests), `crates/cli/tests/backtest_job.rs`,
  `crates/cli/src/jobs/train.rs`, `crates/runtime/src/{bootstrap,cutover,evaluator,vintage_rollover}.rs`
  (tests), `crates/runtime/tests/restart_parity.rs`.
- **Rollback:** revert the branch; no persisted production artefact depends on the new format yet.

## Supersedes

QE-262 (catalogue identity not pinned in the vintage) is fully delivered here and is superseded.
