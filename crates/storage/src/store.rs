//! The LMDB-backed market-data store.

use std::path::Path;

use heed::types::{Bytes, DecodeIgnore, SerdeJson, Str};
use heed::{Database, Env, RoTxn};
use serde::de::DeserializeOwned;

use qe_domain::{Bar, FundingRateSample, InstrumentId, Resolution, Timestamp};

use crate::engine::{check_or_init_schema, open_env, read_schema_version, DB_META};
use crate::key::{bar_key, bar_prefix, series_key, series_prefix, time_from_key};
use crate::records::{FuturesMetrics, PremiumSample};
use crate::{StorageError, SCHEMA_VERSION};

/// Default LMDB map size (max on-disk size): 1 GiB. Real deployments may size up.
pub const DEFAULT_MAP_SIZE: usize = 1 << 30;

const DB_BARS: &str = "bars";
const DB_FUNDING: &str = "funding";
const DB_PREMIUM: &str = "premium";
const DB_FUTURES: &str = "futures_metrics";

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
    futures: Database<Bytes, SerdeJson<FuturesMetrics>>,
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
        let futures: Database<Bytes, SerdeJson<FuturesMetrics>> =
            env.create_database(&mut wtxn, Some(DB_FUTURES))?;

        check_or_init_schema(&meta, &mut wtxn, SCHEMA_VERSION)?;
        wtxn.commit()?;

        Ok(MarketStore {
            env,
            bars,
            funding,
            premium,
            futures,
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
}
