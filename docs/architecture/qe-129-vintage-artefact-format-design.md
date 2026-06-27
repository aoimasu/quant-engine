# QE-129 — Ensemble repository, calibration profile & vintage artefact format — design note

`Phase: P1` · `Area: ⑦ Vintage outputs` · `Depends on: QE-127, QE-116, QE-006`
`Branch: qe-129/vintage-artefact-format`

## Goal (from backlog)

The vintage (chromosomes + ensemble + calibration) is the unit handed to runtime; its format and
versioning underpin reproducibility and rollover.

- Define and write the ensemble repository + per-vintage calibration profile (QE-116) + vintage artefact
  format with a content hash and full lineage.
- Format is read-only-loadable by runtime (QE-219).

**Acceptance criteria.**
- [ ] A vintage round-trips (write → load) and its hash is stable and verifiable.

**Out of scope.** Runtime consumption (QE-219).

## Current-state evidence & placement

- This is the **output** stage (Area ⑦): a vintage bundles the search output (chromosomes — `qe_wfo::Genome`,
  QE-110/123) with the portfolio output (the ensemble's per-chromosome weights — QE-126/127/128) and the
  per-vintage calibration sidecar (`qe_risk::CalibrationProfile`, QE-116, whose own doc already says it
  "rides the vintage artefact (QE-129)"), tagged with a resolvable `qe_determinism::Lineage` (QE-006).
- **A new crate `qe-vintage`** (Area ⑦). It is *downstream* of the search⟂portfolio firewall — the
  firewall governs information flow *during* search/portfolio construction, not a final artefact that
  records their outputs — so it may reference both sides' **data** types. It deliberately stores the
  ensemble as plain **per-chromosome weights** (not `qe_ensemble`'s search types), so the artefact is pure
  data that runtime (QE-219) can load without pulling in any search/portfolio logic.

## Design

### D1 — The artefact: content + content hash

`VintageContent` holds everything that is hashed: `format_version`, a `vintage_id`, the `chromosomes`
(`Vec<Genome>`), the ensemble `weights` (`Vec<f64>`, aligned to chromosomes — the capacity-capped output
of QE-128), the `calibration` (`CalibrationProfile`), and the `lineage` (`Lineage`). Its `content_hash()`
is the lowercase-hex SHA-256 over the record's canonical JSON (`serde_json::to_vec`) — the same
content-hashing pattern as `Lineage::id` (QE-006), so the hash is **stable** (deterministic serialisation:
`BTreeMap`-ordered calibration maps, fixed field order) and **verifiable** (recompute and compare).

`Vintage { content, content_hash }` is the sealed artefact. `Vintage::seal(content)` computes and pins the
hash; `Vintage::verify()` recomputes it and errors on mismatch — so any post-seal tampering with the
content is detected.

### D2 — Write / load (round-trip, AC)

`Vintage::write(w)` serialises the whole sealed artefact as JSON to any `Write`; `Vintage::load(r)`
deserialises from any `Read` **and verifies the content hash** before returning — a load never yields an
unverified vintage. Together they are the round-trip the AC requires; in-memory buffers exercise the exact
serde + hash path, and the filesystem repository (D3) is a thin wrapper.

### D3 — The ensemble/vintage repository

`VintageRepository { root }` is the on-disk store: `write(&vintage)` writes `<root>/<vintage_id>.json`
(creating `root` if needed) and `load(vintage_id)` reads + verifies. Keying by `vintage_id` makes
rollover a directory listing; the content hash inside each file pins reproducibility. Runtime (QE-219)
will open this read-only.

### D4 — Versioning

`format_version` (`VINTAGE_FORMAT_VERSION = 1`) is part of the hashed content, so a format change changes
the hash and is explicit. The lineage records config hash + input snapshot + code commit + seeds, so a
vintage is fully reproducible from its artefact.

## Module / API plan

New crate `crates/vintage` (`qe-vintage`), added to `[workspace.dependencies]` for QE-219:

- `VintageContent { format_version, vintage_id, chromosomes, weights, calibration, lineage }` (serde) +
  `content_hash()`.
- `Vintage { content, content_hash }` (serde) + `seal`, `verify`, `write`, `load`.
- `VintageRepository { root }` + `new`, `write`, `load`, `path_for`.
- `VintageError { Serialize, Deserialize, HashMismatch, Io }` (thiserror), `VINTAGE_FORMAT_VERSION`.
- Deps: `qe-wfo` (Genome), `qe-risk` (CalibrationProfile), `qe-determinism` (Lineage), `serde`,
  `serde_json`, `sha2`, `thiserror`. (Downstream of the firewall — no `qe-ensemble` type dep; the
  ensemble is materialised as weights.)

## Test plan (TDD)

1. **Round-trip + stable, verifiable hash (AC).** Seal a vintage (genomes + weights + calibration +
   lineage), write to a buffer, load back: the loaded vintage equals the sealed one and its
   `content_hash` matches; sealing the same content twice yields the **same** hash.
2. **Tamper detection.** Mutating a sealed vintage's content without re-sealing makes `verify()` (and
   `load`) fail with `HashMismatch` — the hash is genuinely verifiable.
3. **Repository round-trip.** `VintageRepository::write` then `load(vintage_id)` reproduces the verified
   vintage from disk.
4. **Version in the hash.** Changing `format_version` changes the content hash.

## Gates

`cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`, `cargo test -p qe-vintage`,
`cargo test --workspace`.

## Risks

- **`Genome` lives in `qe-wfo`.** Embedding it makes `qe-vintage` (and later runtime) depend on `qe-wfo`
  for the type. A future refactor could move `Genome` to a lower shared crate so "live" needn't depend on
  "search"; out of scope here — QE-129 uses the type where it lives.
- **JSON format.** Human-readable + matches QE-110's canonical genome JSON; a binary codec is a later
  size/speed optimisation behind the same `write`/`load` seam. `serde_json` cannot represent non-finite
  floats, but weights are finite `[0,1]` (capacity-capped).
- **Hash stability depends on canonical serialisation.** Guaranteed here because every embedded type uses
  deterministic serde (fixed field order; `BTreeMap` for the calibration maps). Documented as a contract
  for any future field addition.
