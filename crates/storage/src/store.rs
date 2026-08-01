//! The LMDB-backed market-data store.

use std::path::Path;

use heed::types::{Bytes, DecodeIgnore, SerdeJson, Str};
use heed::{Database, Env, RoTxn};
use serde::de::DeserializeOwned;

use qe_domain::{Bar, FundingRateSample, InstrumentId, Resolution, Timestamp};

use crate::engine::{check_or_init_schema, open_env, read_schema_version, DB_META};
use crate::key::{bar_key, bar_prefix, series_key, series_prefix, time_from_key};
use crate::provenance::{Calibration, Provenance, ProvenanceSegment, ProvenanceSummary};
use crate::records::{FuturesMetrics, OpenInterestSample, PremiumSample};
use crate::{StorageError, SCHEMA_VERSION};

/// Default LMDB map size (max on-disk size): 1 GiB. Real deployments may size up.
pub const DEFAULT_MAP_SIZE: usize = 1 << 30;

const DB_BARS: &str = "bars";
const DB_FUNDING: &str = "funding";
const DB_PREMIUM: &str = "premium";
/// QE-497: aggregate open-interest series (`/futures/data/openInterestHist`), keyed instrument + time.
const DB_OPEN_INTEREST: &str = "open_interest";
const DB_FUTURES: &str = "futures_metrics";
/// QE-464: per-run provenance segments, keyed identically to a bar key by the range start.
const DB_PROVENANCE: &str = "provenance";

/// An embedded LMDB store for the fused market corpus.
///
/// One `Env`, one named sub-database per record kind plus `meta`. Keys are order-preserving
/// (see [`crate::key`]) so range scans are chronological. `Send + Sync`; reads use MVCC snapshot
/// transactions, so many readers run concurrently with a single writer.
pub struct MarketStore {
    env: Env,
    bars: Database<Bytes, SerdeJson<Bar>>,
    funding: Database<Bytes, SerdeJson<FundingRateSample>>,
    premium: Database<Bytes, SerdeJson<PremiumSample>>,
    open_interest: Database<Bytes, SerdeJson<OpenInterestSample>>,
    futures: Database<Bytes, SerdeJson<FuturesMetrics>>,
    provenance: Database<Bytes, SerdeJson<ProvenanceSegment>>,
    meta: Database<Str, Str>,
}

impl MarketStore {
    /// Open (creating if needed) the store at `path` with the given LMDB `map_size`.
    ///
    /// Records [`SCHEMA_VERSION`] on first open; on a later open, a different recorded version is
    /// rejected with [`StorageError::SchemaMismatch`].
    ///
    /// # Caller contract
    /// LMDB maps the file into the process; opening the **same `path` more than once concurrently**
    /// (a second `MarketStore`, or any other `Env`, in this process) is undefined behaviour that the
    /// type system cannot prevent. Keep a single `MarketStore` per path and share it (`Arc`) — it is
    /// `Send + Sync` and supports concurrent reads.
    ///
    /// # Errors
    /// [`StorageError`] on I/O, an LMDB failure, or a schema-version mismatch.
    pub fn open(path: impl AsRef<Path>, map_size: usize) -> Result<Self, StorageError> {
        let env = open_env(path, map_size, 8)?;

        let mut wtxn = env.write_txn()?;
        let meta: Database<Str, Str> = env.create_database(&mut wtxn, Some(DB_META))?;
        let bars: Database<Bytes, SerdeJson<Bar>> =
            env.create_database(&mut wtxn, Some(DB_BARS))?;
        let funding: Database<Bytes, SerdeJson<FundingRateSample>> =
            env.create_database(&mut wtxn, Some(DB_FUNDING))?;
        let premium: Database<Bytes, SerdeJson<PremiumSample>> =
            env.create_database(&mut wtxn, Some(DB_PREMIUM))?;
        // QE-497: open-interest series. Additive like the QE-464 provenance db — a store written before
        // this ticket simply has an empty `open_interest` db (no SCHEMA_VERSION bump, no key/value change
        // to existing records, so already-ingested bars keep their identity).
        let open_interest: Database<Bytes, SerdeJson<OpenInterestSample>> =
            env.create_database(&mut wtxn, Some(DB_OPEN_INTEREST))?;
        let futures: Database<Bytes, SerdeJson<FuturesMetrics>> =
            env.create_database(&mut wtxn, Some(DB_FUTURES))?;
        // QE-464: the provenance index is created on open; a store written before this ticket simply has
        // an empty `provenance` DB, so its bars read `unknown` (documented legacy default — no migration).
        // Additive — no SCHEMA_VERSION bump and no bar key/value change, so already-ingested bars keep
        // their identity (no input_snapshot_id drift).
        let provenance: Database<Bytes, SerdeJson<ProvenanceSegment>> =
            env.create_database(&mut wtxn, Some(DB_PROVENANCE))?;

        check_or_init_schema(&meta, &mut wtxn, SCHEMA_VERSION)?;
        wtxn.commit()?;

        Ok(MarketStore {
            env,
            bars,
            funding,
            premium,
            open_interest,
            futures,
            provenance,
            meta,
        })
    }

    /// The schema version recorded in the store.
    ///
    /// # Errors
    /// [`StorageError`] on an LMDB failure or a corrupt/missing version record.
    pub fn schema_version(&self) -> Result<u32, StorageError> {
        read_schema_version(&self.env, &self.meta)
    }

    // ---- bars (keyed by instrument + resolution + time) -------------------------------------

    /// Insert bars for `instrument` (one write transaction). Each bar carries its own resolution
    /// and open time.
    ///
    /// # Errors
    /// [`StorageError`] on an LMDB failure.
    pub fn put_bars(&self, instrument: &InstrumentId, bars: &[Bar]) -> Result<(), StorageError> {
        let mut wtxn = self.env.write_txn()?;
        for bar in bars {
            let key = bar_key(instrument, bar.resolution(), bar.open_time());
            self.bars.put(&mut wtxn, &key, bar)?;
        }
        wtxn.commit()?;
        Ok(())
    }

    /// Fetch a single bar by exact key.
    ///
    /// # Errors
    /// [`StorageError`] on an LMDB failure.
    pub fn get_bar(
        &self,
        instrument: &InstrumentId,
        resolution: Resolution,
        time: Timestamp,
    ) -> Result<Option<Bar>, StorageError> {
        let rtxn = self.env.read_txn()?;
        Ok(self
            .bars
            .get(&rtxn, &bar_key(instrument, resolution, time))?)
    }

    /// Scan bars for `instrument`+`resolution` over `[from, to)`, in chronological order.
    ///
    /// # Errors
    /// [`StorageError`] on an LMDB failure.
    pub fn scan_bars(
        &self,
        instrument: &InstrumentId,
        resolution: Resolution,
        from: Timestamp,
        to: Timestamp,
    ) -> Result<Vec<Bar>, StorageError> {
        let rtxn = self.env.read_txn()?;
        scan_series(
            &self.bars,
            &rtxn,
            &bar_prefix(instrument, resolution),
            from,
            to,
        )
    }

    /// Distinct instruments that have at least one stored bar, in **ascending key order**.
    ///
    /// The market store has no separate instrument index, so this iterates the `bars` DB keys and
    /// recovers each symbol (the bytes before the first `0x00` delimiter — see [`crate::key`]).
    /// Keys iterate in lexicographic (= symbol-grouped, ascending) order, so a running-last dedupe
    /// yields the distinct instruments deterministically. Any key whose symbol prefix is not a valid
    /// [`InstrumentId`] is skipped defensively (the writer never produces one). Bar values are not
    /// decoded (the data type is remapped to raw bytes), so this stays a cheap key-only scan.
    ///
    /// # Errors
    /// [`StorageError`] on an LMDB failure.
    pub fn bar_instruments(&self) -> Result<Vec<InstrumentId>, StorageError> {
        let rtxn = self.env.read_txn()?;
        let mut out: Vec<InstrumentId> = Vec::new();
        for item in self.bars.remap_data_type::<Bytes>().iter(&rtxn)? {
            let (key, _) = item?;
            let end = key.iter().position(|&b| b == 0).unwrap_or(key.len());
            let Ok(symbol) = std::str::from_utf8(&key[..end]) else {
                continue;
            };
            let Ok(instrument) = InstrumentId::new(symbol) else {
                continue;
            };
            if out.last() != Some(&instrument) {
                out.push(instrument);
            }
        }
        Ok(out)
    }

    /// Covered range + bar count for one `(instrument, resolution)` pair, **without decoding any
    /// `Bar` value** (QE-412).
    ///
    /// Returns `Some((first_open_time, last_open_time, count))` — the earliest and latest bar
    /// `open_time` (inclusive) and the number of stored bars — or `None` when the pair has no bars.
    ///
    /// Bars share the `(instrument, resolution)` key prefix and sort chronologically (the key ends in
    /// an order-preserving timestamp, see [`crate::key`]), so the first key in the prefix carries the
    /// earliest open time and the last key the latest. The value type is remapped to
    /// [`heed::types::DecodeIgnore`] — whose decoder returns `()` without reading the value bytes — so
    /// the `SerdeJson<Bar>` deserialiser is **never** invoked: timestamps come purely from the key.
    /// This is the key-only cursor [`Self::bar_instruments`] uses, specialised to one prefix.
    ///
    /// # Errors
    /// [`StorageError`] on an LMDB failure.
    pub fn coverage_bounds(
        &self,
        instrument: &InstrumentId,
        resolution: Resolution,
    ) -> Result<Option<(Timestamp, Timestamp, usize)>, StorageError> {
        let rtxn = self.env.read_txn()?;
        let prefix = bar_prefix(instrument, resolution);
        let mut first: Option<i64> = None;
        let mut last: i64 = 0;
        let mut count: usize = 0;
        for item in self
            .bars
            .remap_data_type::<DecodeIgnore>()
            .prefix_iter(&rtxn, &prefix)?
        {
            let (key, ()) = item?;
            let t = time_from_key(key).millis();
            if first.is_none() {
                first = Some(t);
            }
            last = t;
            count += 1;
        }
        Ok(first.map(|f| {
            (
                Timestamp::from_millis(f),
                Timestamp::from_millis(last),
                count,
            )
        }))
    }

    /// Number of stored bars for `(instrument, resolution)` whose open-time falls in the **inclusive**
    /// range `[from, to]`, counted from **keys only** (no `Bar` value decoded — QE-412), so the
    /// per-provenance-segment coverage split stays key-only.
    ///
    /// # Errors
    /// [`StorageError`] on an LMDB failure.
    pub fn count_bars_in_range(
        &self,
        instrument: &InstrumentId,
        resolution: Resolution,
        from: Timestamp,
        to: Timestamp,
    ) -> Result<usize, StorageError> {
        let rtxn = self.env.read_txn()?;
        let prefix = bar_prefix(instrument, resolution);
        let mut count = 0usize;
        for item in self
            .bars
            .remap_data_type::<DecodeIgnore>()
            .prefix_iter(&rtxn, &prefix)?
        {
            let (key, ()) = item?;
            let t = time_from_key(key).millis();
            if t >= from.millis() && t <= to.millis() {
                count += 1;
            }
        }
        Ok(count)
    }

    // ---- provenance (QE-464: per-run real/synthetic/calibration segments) --------------------

    /// Insert `bars` for `instrument` and record their origin as one provenance segment spanning the
    /// `[first, last]` open-time of the batch.
    ///
    /// The bars are written through the exact same path as [`Self::put_bars`] (so bar identity — and any
    /// snapshot id over bar bytes — is unchanged); the segment is written to the **separate** provenance
    /// index, keyed by the batch's first open-time. An empty batch writes nothing. Re-tagging a range
    /// (writing a later segment) never touches bar keys/values, so already-ingested data does not drift.
    ///
    /// **Atomicity (QE-485).** The bar puts and their per-resolution provenance segments commit in a
    /// **single** write transaction — they are all-or-nothing. A crash, panic, or process kill can never
    /// leave real bars durably persisted with no covering segment (which the provenance fold would then
    /// read as [`Provenance::Unknown`]/`Mixed` and permanently mislabel, since incremental ingest never
    /// re-fetches already-covered slots). The bar key/value path is byte-identical to [`Self::put_bars`];
    /// only the transaction boundary differs (one commit instead of two).
    ///
    /// # Errors
    /// [`StorageError`] on an LMDB failure.
    pub fn put_bars_with_provenance(
        &self,
        instrument: &InstrumentId,
        bars: &[Bar],
        provenance: Provenance,
        calibration: Calibration,
    ) -> Result<(), StorageError> {
        // Bars and their provenance segments share ONE wtxn (QE-485): the tag is present the instant the
        // bars are. Same bar key/value path as `put_bars`, written into this txn rather than its own.
        let mut wtxn = self.env.write_txn()?;
        for bar in bars {
            let key = bar_key(instrument, bar.resolution(), bar.open_time());
            self.bars.put(&mut wtxn, &key, bar)?;
        }
        // One segment per (instrument, resolution) present in the batch: bars can carry mixed resolutions.
        for resolution in Resolution::ALL {
            let times: Vec<Timestamp> = bars
                .iter()
                .filter(|b| b.resolution() == resolution)
                .map(Bar::open_time)
                .collect();
            let (Some(first), Some(last)) = (times.iter().min(), times.iter().max()) else {
                continue;
            };
            let seg = ProvenanceSegment {
                end_ms: last.millis(),
                provenance,
                calibration,
            };
            self.provenance
                .put(&mut wtxn, &bar_key(instrument, resolution, *first), &seg)?;
        }
        wtxn.commit()?;
        Ok(())
    }

    /// The provenance segments recorded for one `(instrument, resolution)` pair, ascending by range start.
    ///
    /// Each entry is `(start_open_time, end_open_time, provenance, calibration)`. Prefix-scans the
    /// provenance index by key (the bars DB is untouched, so [`Self::coverage_bounds`] stays key-only).
    ///
    /// # Errors
    /// [`StorageError`] on an LMDB failure.
    pub fn provenance_segments(
        &self,
        instrument: &InstrumentId,
        resolution: Resolution,
    ) -> Result<Vec<(Timestamp, Timestamp, Provenance, Calibration)>, StorageError> {
        let rtxn = self.env.read_txn()?;
        let prefix = bar_prefix(instrument, resolution);
        let mut out = Vec::new();
        for item in self.provenance.prefix_iter(&rtxn, &prefix)? {
            let (key, seg) = item?;
            out.push((
                time_from_key(key),
                Timestamp::from_millis(seg.end_ms),
                seg.provenance,
                seg.calibration,
            ));
        }
        Ok(out)
    }

    /// Fold the provenance of every stored bar of `instruments` at `resolution` into a single
    /// [`ProvenanceSummary`] (the verdict the train path maps onto `qe_vintage::DataProvenance`).
    ///
    /// A `(instrument, resolution)` that has bars but **no** provenance segment contributes an
    /// [`Provenance::Unknown`] (legacy/untagged) so a partially-tagged store never reports as fully
    /// real. Instruments with no bars contribute nothing.
    ///
    /// # Errors
    /// [`StorageError`] on an LMDB failure.
    pub fn store_provenance_summary(
        &self,
        instruments: &[InstrumentId],
        resolution: Resolution,
    ) -> Result<ProvenanceSummary, StorageError> {
        let mut provenances = Vec::new();
        for instrument in instruments {
            let segments = self.provenance_segments(instrument, resolution)?;
            if segments.is_empty() {
                // Bars present but untagged → legacy unknown; no bars → contribute nothing.
                if self.coverage_bounds(instrument, resolution)?.is_some() {
                    provenances.push(Provenance::Unknown);
                }
            } else {
                provenances.extend(segments.iter().map(|&(_, _, p, _)| p));
            }
        }
        Ok(ProvenanceSummary::from_provenances(provenances))
    }

    // ---- funding (keyed by instrument + time) -----------------------------------------------

    /// Insert funding-rate samples (one write transaction).
    ///
    /// # Errors
    /// [`StorageError`] on an LMDB failure.
    pub fn put_funding(&self, samples: &[FundingRateSample]) -> Result<(), StorageError> {
        let mut wtxn = self.env.write_txn()?;
        for s in samples {
            self.funding
                .put(&mut wtxn, &series_key(&s.instrument, s.time), s)?;
        }
        wtxn.commit()?;
        Ok(())
    }

    /// Fetch a funding sample by exact key.
    ///
    /// # Errors
    /// [`StorageError`] on an LMDB failure.
    pub fn get_funding(
        &self,
        instrument: &InstrumentId,
        time: Timestamp,
    ) -> Result<Option<FundingRateSample>, StorageError> {
        let rtxn = self.env.read_txn()?;
        Ok(self.funding.get(&rtxn, &series_key(instrument, time))?)
    }

    /// Scan funding samples for `instrument` over `[from, to)`.
    ///
    /// # Errors
    /// [`StorageError`] on an LMDB failure.
    pub fn scan_funding(
        &self,
        instrument: &InstrumentId,
        from: Timestamp,
        to: Timestamp,
    ) -> Result<Vec<FundingRateSample>, StorageError> {
        let rtxn = self.env.read_txn()?;
        scan_series(&self.funding, &rtxn, &series_prefix(instrument), from, to)
    }

    // ---- premium / spread-to-underlier (keyed by instrument + time) -------------------------

    /// Insert premium/spread samples (one write transaction).
    ///
    /// # Errors
    /// [`StorageError`] on an LMDB failure.
    pub fn put_premium(&self, samples: &[PremiumSample]) -> Result<(), StorageError> {
        let mut wtxn = self.env.write_txn()?;
        for s in samples {
            self.premium
                .put(&mut wtxn, &series_key(&s.instrument, s.time), s)?;
        }
        wtxn.commit()?;
        Ok(())
    }

    /// Fetch a premium sample by exact key.
    ///
    /// # Errors
    /// [`StorageError`] on an LMDB failure.
    pub fn get_premium(
        &self,
        instrument: &InstrumentId,
        time: Timestamp,
    ) -> Result<Option<PremiumSample>, StorageError> {
        let rtxn = self.env.read_txn()?;
        Ok(self.premium.get(&rtxn, &series_key(instrument, time))?)
    }

    /// Scan premium samples for `instrument` over `[from, to)`.
    ///
    /// # Errors
    /// [`StorageError`] on an LMDB failure.
    pub fn scan_premium(
        &self,
        instrument: &InstrumentId,
        from: Timestamp,
        to: Timestamp,
    ) -> Result<Vec<PremiumSample>, StorageError> {
        let rtxn = self.env.read_txn()?;
        scan_series(&self.premium, &rtxn, &series_prefix(instrument), from, to)
    }

    // ---- open interest (keyed by instrument + time) -----------------------------------------

    /// Insert open-interest samples (one write transaction) — QE-497.
    ///
    /// Mirrors [`Self::put_premium`]: keyed by `(instrument, time)` under the `open_interest` db, one
    /// write transaction (QE-485 single-wtxn discipline), so a batch lands atomically.
    ///
    /// # Errors
    /// [`StorageError`] on an LMDB failure.
    pub fn put_open_interest(&self, samples: &[OpenInterestSample]) -> Result<(), StorageError> {
        let mut wtxn = self.env.write_txn()?;
        for s in samples {
            self.open_interest
                .put(&mut wtxn, &series_key(&s.instrument, s.time), s)?;
        }
        wtxn.commit()?;
        Ok(())
    }

    /// Fetch an open-interest sample by exact key.
    ///
    /// # Errors
    /// [`StorageError`] on an LMDB failure.
    pub fn get_open_interest(
        &self,
        instrument: &InstrumentId,
        time: Timestamp,
    ) -> Result<Option<OpenInterestSample>, StorageError> {
        let rtxn = self.env.read_txn()?;
        Ok(self
            .open_interest
            .get(&rtxn, &series_key(instrument, time))?)
    }

    /// Scan open-interest samples for `instrument` over `[from, to)`.
    ///
    /// # Errors
    /// [`StorageError`] on an LMDB failure.
    pub fn scan_open_interest(
        &self,
        instrument: &InstrumentId,
        from: Timestamp,
        to: Timestamp,
    ) -> Result<Vec<OpenInterestSample>, StorageError> {
        let rtxn = self.env.read_txn()?;
        scan_series(
            &self.open_interest,
            &rtxn,
            &series_prefix(instrument),
            from,
            to,
        )
    }

    // ---- futures metrics (keyed by instrument + time) ---------------------------------------

    /// Insert futures-metrics samples (one write transaction).
    ///
    /// # Errors
    /// [`StorageError`] on an LMDB failure.
    pub fn put_futures(&self, samples: &[FuturesMetrics]) -> Result<(), StorageError> {
        let mut wtxn = self.env.write_txn()?;
        for s in samples {
            self.futures
                .put(&mut wtxn, &series_key(&s.instrument, s.time), s)?;
        }
        wtxn.commit()?;
        Ok(())
    }

    /// Fetch a futures-metrics sample by exact key.
    ///
    /// # Errors
    /// [`StorageError`] on an LMDB failure.
    pub fn get_futures(
        &self,
        instrument: &InstrumentId,
        time: Timestamp,
    ) -> Result<Option<FuturesMetrics>, StorageError> {
        let rtxn = self.env.read_txn()?;
        Ok(self.futures.get(&rtxn, &series_key(instrument, time))?)
    }

    /// Scan futures-metrics samples for `instrument` over `[from, to)`.
    ///
    /// # Errors
    /// [`StorageError`] on an LMDB failure.
    pub fn scan_futures(
        &self,
        instrument: &InstrumentId,
        from: Timestamp,
        to: Timestamp,
    ) -> Result<Vec<FuturesMetrics>, StorageError> {
        let rtxn = self.env.read_txn()?;
        scan_series(&self.futures, &rtxn, &series_prefix(instrument), from, to)
    }

    // ---- vintage lineage ledger (QE-105) ----------------------------------------------------

    /// Record that the vintage `lineage_id` has been persisted into this store.
    ///
    /// Returns `true` if the id was newly recorded, `false` if it was already present — letting a
    /// persist step skip re-writing an already-persisted vintage (idempotency keyed by lineage).
    /// Stored in the `meta` db under a `lineage:` prefix, so it cannot collide with the
    /// schema-version key.
    ///
    /// # Errors
    /// [`StorageError`] on an LMDB failure.
    pub fn record_lineage(&self, lineage_id: &str) -> Result<bool, StorageError> {
        let key = format!("{LINEAGE_PREFIX}{lineage_id}");
        let mut wtxn = self.env.write_txn()?;
        let existed = self.meta.get(&wtxn, &key)?.is_some();
        if !existed {
            self.meta.put(&mut wtxn, &key, "1")?;
        }
        wtxn.commit()?;
        Ok(!existed)
    }

    /// Whether `lineage_id` has already been recorded as persisted.
    ///
    /// # Errors
    /// [`StorageError`] on an LMDB failure.
    pub fn has_lineage(&self, lineage_id: &str) -> Result<bool, StorageError> {
        let rtxn = self.env.read_txn()?;
        Ok(self
            .meta
            .get(&rtxn, &format!("{LINEAGE_PREFIX}{lineage_id}"))?
            .is_some())
    }

    /// All vintage lineage ids recorded in this store, ascending.
    ///
    /// # Errors
    /// [`StorageError`] on an LMDB failure.
    pub fn lineages(&self) -> Result<Vec<String>, StorageError> {
        let rtxn = self.env.read_txn()?;
        let mut out = Vec::new();
        for item in self.meta.prefix_iter(&rtxn, LINEAGE_PREFIX)? {
            let (key, _) = item?;
            if let Some(id) = key.strip_prefix(LINEAGE_PREFIX) {
                out.push(id.to_owned());
            }
        }
        Ok(out)
    }
}

/// `meta`-db key prefix for the vintage lineage ledger (QE-105).
const LINEAGE_PREFIX: &str = "lineage:";

/// Prefix-scan a database, returning values whose key-time falls in `[from, to)`, in order.
///
/// Keys under one prefix are chronological, so the scan stops as soon as it passes `to`.
fn scan_series<T>(
    db: &Database<Bytes, SerdeJson<T>>,
    rtxn: &RoTxn,
    prefix: &[u8],
    from: Timestamp,
    to: Timestamp,
) -> Result<Vec<T>, StorageError>
where
    T: DeserializeOwned + 'static,
{
    let mut out = Vec::new();
    for item in db.prefix_iter(rtxn, prefix)? {
        let (key, value) = item?;
        let t = time_from_key(key).millis();
        if t >= to.millis() {
            break;
        }
        if t >= from.millis() {
            out.push(value);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coverage::{coverage, CoverageRow};
    use crate::key::bar_key;
    use qe_domain::{Price, Qty};
    use rust_decimal::Decimal;

    fn inst(s: &str) -> InstrumentId {
        InstrumentId::new(s).unwrap()
    }

    fn price(n: i64) -> Price {
        Price::new(Decimal::from(n)).unwrap()
    }

    fn bar(res: Resolution, secs: i64, base: i64) -> Bar {
        Bar::new(
            Timestamp::from_secs(secs),
            res,
            price(base),
            price(base + 10),
            price(base - 10),
            price(base),
            Qty::new(Decimal::from(1)).unwrap(),
            1,
        )
        .unwrap()
    }

    fn open(dir: &std::path::Path) -> MarketStore {
        MarketStore::open(dir, 10 * 1024 * 1024).unwrap()
    }

    #[test]
    fn coverage_bounds_reports_range_and_count_from_keys() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        let id = inst("BTCUSDT");

        // Empty prefix → None.
        assert_eq!(store.coverage_bounds(&id, Resolution::M5).unwrap(), None);

        store
            .put_bars(
                &id,
                &[
                    bar(Resolution::M5, 100, 100),
                    bar(Resolution::M5, 200, 110),
                    bar(Resolution::M5, 300, 120),
                ],
            )
            .unwrap();
        // Single-bar H1 (first == last, count == 1) — a separate resolution prefix.
        store
            .put_bars(&id, &[bar(Resolution::H1, 3600, 200)])
            .unwrap();

        assert_eq!(
            store.coverage_bounds(&id, Resolution::M5).unwrap(),
            Some((
                Timestamp::from_millis(100_000),
                Timestamp::from_millis(300_000),
                3
            )),
        );
        assert_eq!(
            store.coverage_bounds(&id, Resolution::H1).unwrap(),
            Some((
                Timestamp::from_millis(3_600_000),
                Timestamp::from_millis(3_600_000),
                1
            )),
        );
        // A resolution with no bars stays None.
        assert_eq!(store.coverage_bounds(&id, Resolution::D1).unwrap(), None);
    }

    /// QE-412 AC #2: the coverage path (`coverage_bounds` → `coverage`) reads timestamps + count from
    /// the KEY only and never decodes a `Bar` value. Proven by planting a bar whose value bytes are
    /// **undecodable JSON** under a valid `bar_key`: `coverage_bounds` still returns the correct
    /// range/count, while `scan_bars` (which *does* decode values) errors on the very same store — so
    /// the coverage success is not vacuous.
    #[test]
    fn coverage_path_never_decodes_bar_values() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        let id = inst("BTCUSDT");
        let res = Resolution::M5;

        // Two real bars, then overwrite the second bar's *value* with non-JSON bytes while keeping its
        // valid key (bypassing `SerdeJson<Bar>` via a raw-bytes remap — the only way to forge an
        // undecodable value the writer would never produce).
        store
            .put_bars(&id, &[bar(res, 100, 100), bar(res, 200, 110)])
            .unwrap();
        {
            let mut wtxn = store.env.write_txn().unwrap();
            store
                .bars
                .remap_data_type::<Bytes>()
                .put(
                    &mut wtxn,
                    &bar_key(&id, res, Timestamp::from_secs(200)),
                    &b"\xffnot-json"[..],
                )
                .unwrap();
            wtxn.commit().unwrap();
        }

        // Coverage path succeeds with correct range + count: no value was decoded.
        assert_eq!(
            store.coverage_bounds(&id, res).unwrap(),
            Some((
                Timestamp::from_millis(100_000),
                Timestamp::from_millis(200_000),
                2
            )),
        );
        assert_eq!(
            coverage(&store, std::slice::from_ref(&id)).unwrap(),
            vec![CoverageRow {
                symbol: "BTCUSDT".to_owned(),
                resolution: res.as_str().to_owned(),
                from: 100_000,
                to: 200_000,
                bars: 2,
                // No provenance segment was written (raw `put_bars`) ⇒ legacy `unknown`.
                provenance: "unknown".to_owned(),
                calibrated: false,
            }],
        );

        // Control: the decode path *does* choke on the same store, proving the value is genuinely
        // undecodable — so the coverage success above is meaningful, not vacuous.
        assert!(
            store
                .scan_bars(
                    &id,
                    res,
                    Timestamp::from_millis(i64::MIN),
                    Timestamp::from_millis(i64::MAX)
                )
                .is_err(),
            "scan_bars decodes Bar values and must fail on the planted garbage value",
        );
    }

    /// QE-497: open-interest and premium round-trip through the store — `put_*` then `scan_*` returns
    /// exactly the persisted samples in chronological order, and the two series are keyed independently
    /// (an OI write does not appear in a premium scan or vice-versa).
    #[test]
    fn open_interest_and_premium_round_trip_independently() {
        use crate::records::{OpenInterestSample, PremiumSample};
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        let id = inst("BTCUSDT");
        let other = inst("ETHUSDT");

        let oi: Vec<OpenInterestSample> = [(100i64, 70_000i64), (200, 71_000), (300, 69_500)]
            .into_iter()
            .map(|(secs, v)| OpenInterestSample {
                instrument: id.clone(),
                time: Timestamp::from_secs(secs),
                open_interest: Decimal::from(v),
            })
            .collect();
        let premium: Vec<PremiumSample> = [(100i64, 5i64), (200, -3), (300, 8)]
            .into_iter()
            .map(|(secs, v)| PremiumSample {
                instrument: id.clone(),
                time: Timestamp::from_secs(secs),
                premium: Decimal::new(v, 4),
            })
            .collect();

        store.put_open_interest(&oi).unwrap();
        store.put_premium(&premium).unwrap();

        // Exact-key get.
        assert_eq!(
            store
                .get_open_interest(&id, Timestamp::from_secs(200))
                .unwrap(),
            Some(oi[1].clone())
        );
        // Full range scan returns all three in order (half-open `[from, to)`).
        let scanned = store
            .scan_open_interest(&id, Timestamp::from_secs(0), Timestamp::from_secs(1000))
            .unwrap();
        assert_eq!(scanned, oi);
        let scanned_p = store
            .scan_premium(&id, Timestamp::from_secs(0), Timestamp::from_secs(1000))
            .unwrap();
        assert_eq!(scanned_p, premium);

        // Independent keying: a different instrument sees nothing; the two dbs do not bleed.
        assert!(store
            .scan_open_interest(&other, Timestamp::from_secs(0), Timestamp::from_secs(1000))
            .unwrap()
            .is_empty());
        assert_eq!(
            store
                .get_open_interest(&id, Timestamp::from_secs(999))
                .unwrap(),
            None
        );
    }

    #[test]
    fn provenance_tags_survive_and_summarise() {
        use crate::provenance::{Calibration, Provenance, ProvenanceSummary};
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        let id = inst("BTCUSDT");
        let res = Resolution::M5;

        store
            .put_bars_with_provenance(
                &id,
                &[bar(res, 100, 100), bar(res, 200, 110)],
                Provenance::Synthetic,
                Calibration::Uncalibrated,
            )
            .unwrap();

        let segs = store.provenance_segments(&id, res).unwrap();
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].2, Provenance::Synthetic);
        assert_eq!(segs[0].3, Calibration::Uncalibrated);
        // A synthetic store summarises Synthetic — never silently Real.
        assert_eq!(
            store
                .store_provenance_summary(std::slice::from_ref(&id), res)
                .unwrap(),
            ProvenanceSummary::Synthetic
        );
        // Coverage exposes the tag; the bars scan is still key-only.
        let rows = coverage(&store, std::slice::from_ref(&id)).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].provenance, "synthetic");
        assert!(!rows[0].calibrated);
        assert_eq!(rows[0].bars, 2);
    }

    #[test]
    fn mixed_store_reports_multiple_contiguous_rows_never_blended() {
        use crate::provenance::{Calibration, Provenance};
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        let id = inst("BTCUSDT");
        let res = Resolution::M5;

        // Real run over [100,200], then a synthetic run over a later, disjoint range [300,400].
        store
            .put_bars_with_provenance(
                &id,
                &[bar(res, 100, 100), bar(res, 200, 110)],
                Provenance::Real,
                Calibration::Uncalibrated,
            )
            .unwrap();
        store
            .put_bars_with_provenance(
                &id,
                &[bar(res, 300, 120), bar(res, 400, 130)],
                Provenance::Synthetic,
                Calibration::Uncalibrated,
            )
            .unwrap();

        let rows = coverage(&store, std::slice::from_ref(&id)).unwrap();
        // Two contiguous per-provenance rows, one per run — never one blended row.
        assert_eq!(rows.len(), 2, "mixed store must be multiple rows: {rows:?}");
        assert_eq!(rows[0].provenance, "real");
        assert_eq!(
            (rows[0].from, rows[0].to, rows[0].bars),
            (100_000, 200_000, 2)
        );
        assert_eq!(rows[1].provenance, "synthetic");
        assert_eq!(
            (rows[1].from, rows[1].to, rows[1].bars),
            (300_000, 400_000, 2)
        );

        assert_eq!(
            store.store_provenance_summary(&[id], res).unwrap(),
            crate::provenance::ProvenanceSummary::Mixed
        );
    }

    #[test]
    fn retagging_provenance_does_not_drift_bar_identity() {
        use crate::provenance::{Calibration, Provenance};
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        let id = inst("BTCUSDT");
        let res = Resolution::M5;

        let bars = [bar(res, 100, 100), bar(res, 200, 110)];
        store.put_bars(&id, &bars).unwrap();
        let before = store.coverage_bounds(&id, res).unwrap();
        let bar_before = store.get_bar(&id, res, Timestamp::from_secs(100)).unwrap();

        // Re-tag the same range with provenance (rewrites the bars + writes a segment). Bar identity
        // (key-derived bounds + the decoded bar value) must be unchanged — no input_snapshot_id drift.
        store
            .put_bars_with_provenance(&id, &bars, Provenance::Real, Calibration::Uncalibrated)
            .unwrap();
        assert_eq!(store.coverage_bounds(&id, res).unwrap(), before);
        assert_eq!(
            store.get_bar(&id, res, Timestamp::from_secs(100)).unwrap(),
            bar_before
        );
    }

    /// QE-485: bars and their provenance segment commit atomically. After
    /// `put_bars_with_provenance`, every `(instrument, resolution)` that has stored bars ALWAYS has a
    /// covering provenance segment — real bars are never observed as `Unknown`. The single-txn boundary
    /// (one `wtxn`, one `commit`) means there is no code path that persists bars without their segment;
    /// this test asserts the observable invariant across every resolution present in a mixed batch.
    #[test]
    fn put_bars_with_provenance_commits_bars_and_segments_atomically() {
        use crate::provenance::{Calibration, Provenance, ProvenanceSummary};
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        let id = inst("BTCUSDT");

        // A batch spanning two resolutions — each must end up with a covering segment.
        store
            .put_bars_with_provenance(
                &id,
                &[
                    bar(Resolution::M5, 100, 100),
                    bar(Resolution::M5, 200, 110),
                    bar(Resolution::H1, 3600, 200),
                ],
                Provenance::Real,
                Calibration::Uncalibrated,
            )
            .unwrap();

        // Invariant: for EVERY resolution, bars present ⟺ a covering segment present. Never bars-without-tag.
        for res in Resolution::ALL {
            let has_bars = store.coverage_bounds(&id, res).unwrap().is_some();
            let has_segment = !store.provenance_segments(&id, res).unwrap().is_empty();
            assert_eq!(
                has_bars, has_segment,
                "{res:?}: bars and covering segment must be all-or-nothing",
            );
            // A stored bar is never folded as legacy `Unknown` — the segment is always there.
            if has_bars {
                assert_eq!(
                    store
                        .store_provenance_summary(std::slice::from_ref(&id), res)
                        .unwrap(),
                    ProvenanceSummary::Real,
                    "{res:?}: real bars must summarise Real, never Unknown",
                );
            }
        }
    }

    /// QE-485 AC #2: incremental ingest over already-covered slots is unaffected by the txn merge.
    /// Re-invoking with an identical batch neither corrupts bars nor duplicates/mislabels the segment,
    /// and the key-only coverage bounds (the re-fetch short-circuit input) stay byte-identical.
    #[test]
    fn reingesting_covered_slots_does_not_corrupt_bars_or_segments() {
        use crate::provenance::{Calibration, Provenance};
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        let id = inst("BTCUSDT");
        let res = Resolution::M5;
        let bars = [bar(res, 100, 100), bar(res, 200, 110)];

        store
            .put_bars_with_provenance(&id, &bars, Provenance::Real, Calibration::Uncalibrated)
            .unwrap();
        let bounds_first = store.coverage_bounds(&id, res).unwrap();
        let segs_first = store.provenance_segments(&id, res).unwrap();
        let bar_first = store.get_bar(&id, res, Timestamp::from_secs(100)).unwrap();

        // Re-ingest the identical, already-covered batch.
        store
            .put_bars_with_provenance(&id, &bars, Provenance::Real, Calibration::Uncalibrated)
            .unwrap();

        // Coverage (the re-fetch short-circuit input), the single segment, and bar identity are unchanged.
        assert_eq!(store.coverage_bounds(&id, res).unwrap(), bounds_first);
        let segs_again = store.provenance_segments(&id, res).unwrap();
        assert_eq!(
            segs_again.len(),
            1,
            "one segment, not duplicated: {segs_again:?}"
        );
        assert_eq!(
            segs_again, segs_first,
            "segment must not drift on re-ingest"
        );
        assert_eq!(
            store.get_bar(&id, res, Timestamp::from_secs(100)).unwrap(),
            bar_first,
        );
    }
}
