//! The LMDB-backed market-data store.

use std::path::Path;

use heed::types::{Bytes, SerdeJson, Str};
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
