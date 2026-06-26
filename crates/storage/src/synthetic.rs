//! The LMDB-backed synthetic-data store: indicator-state cache + multi-resolution bars, with
//! cache invalidation tied to source lineage (QE-006).

use std::path::Path;

use heed::types::{Bytes, SerdeJson, Str};
use heed::{Database, Env};
use serde::{Deserialize, Serialize};

use qe_domain::{Bar, InstrumentId, Resolution, Timestamp};

use crate::engine::{check_or_init_schema, open_env, read_schema_version, DB_META};
use crate::key::{bar_key, bar_prefix, indicator_key, time_from_key};
use crate::StorageError;

/// On-disk schema version for the synthetic store.
pub const SYNTHETIC_SCHEMA_VERSION: u32 = 1;

const DB_INDICATORS: &str = "indicators";
const DB_RECON_BARS: &str = "recon_bars";

/// Identifies an indicator-state cache entry: instrument + resolution + indicator id + lookback +
/// time. Borrowed for cheap lookups.
#[derive(Debug, Clone, Copy)]
pub struct IndicatorKey<'a> {
    /// The instrument.
    pub instrument: &'a InstrumentId,
    /// The bar resolution the indicator was computed on.
    pub resolution: Resolution,
    /// The indicator identifier (e.g. `"ema"`, `"rsi"`).
    pub indicator_id: &'a str,
    /// The indicator's lookback window length.
    pub lookback: u32,
    /// The bar time the state is for.
    pub time: Timestamp,
}

impl IndicatorKey<'_> {
    fn encode(&self) -> Vec<u8> {
        indicator_key(
            self.instrument,
            self.resolution,
            self.indicator_id,
            self.lookback,
            self.time,
        )
    }
}

/// A reconstructed (coarser-resolution) bar tagged with the source lineage it was derived from.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ReconBar {
    source_lineage: String,
    bar: Bar,
}

/// An embedded LMDB store for derived artefacts: indicator-state cache and multi-resolution bars.
///
/// Every entry is tagged with the **source lineage id** (QE-006 `Lineage::id`, an opaque string)
/// it was derived from. A read supplies the *current* source lineage; an entry whose stored lineage
/// differs is **stale** — detected and not served — and can be evicted in bulk. `Send + Sync` with
/// MVCC concurrent reads.
pub struct SyntheticStore {
    env: Env,
    indicators: Database<Bytes, Bytes>,
    recon_bars: Database<Bytes, SerdeJson<ReconBar>>,
    meta: Database<Str, Str>,
}

impl SyntheticStore {
    /// Open (creating if needed) the synthetic store at `path` with the given LMDB `map_size`.
    ///
    /// # Caller contract
    /// As with [`crate::MarketStore`], keep a single `SyntheticStore` per path per process and share
    /// it (`Arc`); opening the same path twice concurrently is LMDB UB the type can't prevent.
    ///
    /// # Errors
    /// [`StorageError`] on I/O, an LMDB failure, or a schema-version mismatch.
    pub fn open(path: impl AsRef<Path>, map_size: usize) -> Result<Self, StorageError> {
        let env = open_env(path, map_size, 8)?;

        let mut wtxn = env.write_txn()?;
        let meta: Database<Str, Str> = env.create_database(&mut wtxn, Some(DB_META))?;
        let indicators: Database<Bytes, Bytes> =
            env.create_database(&mut wtxn, Some(DB_INDICATORS))?;
        let recon_bars: Database<Bytes, SerdeJson<ReconBar>> =
            env.create_database(&mut wtxn, Some(DB_RECON_BARS))?;

        check_or_init_schema(&meta, &mut wtxn, SYNTHETIC_SCHEMA_VERSION)?;
        wtxn.commit()?;

        Ok(SyntheticStore {
            env,
            indicators,
            recon_bars,
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

    // ---- indicator-state cache --------------------------------------------------------------

    /// Cache an indicator `state` (opaque bytes) under `key`, tagged with the `source_lineage` it was
    /// derived from.
    ///
    /// # Errors
    /// [`StorageError`] on an LMDB failure.
    pub fn put_indicator_state(
        &self,
        key: &IndicatorKey,
        source_lineage: &str,
        state: &[u8],
    ) -> Result<(), StorageError> {
        let value = encode_cache_value(source_lineage, state);
        let mut wtxn = self.env.write_txn()?;
        self.indicators.put(&mut wtxn, &key.encode(), &value)?;
        wtxn.commit()?;
        Ok(())
    }

    /// Fetch the cached state for `key` **iff** it was derived from `current_lineage`.
    ///
    /// Returns `None` when the entry is absent or **stale** (its source lineage differs) — a stale
    /// entry is detected and not served, so the caller recomputes.
    ///
    /// # Errors
    /// [`StorageError`] on an LMDB failure.
    pub fn get_indicator_state(
        &self,
        key: &IndicatorKey,
        current_lineage: &str,
    ) -> Result<Option<Vec<u8>>, StorageError> {
        let rtxn = self.env.read_txn()?;
        match self.indicators.get(&rtxn, &key.encode())? {
            Some(bytes) => match decode_cache_value(bytes) {
                Some((lineage, state)) if lineage == current_lineage => Ok(Some(state.to_vec())),
                _ => Ok(None), // stale (or unparseable) → miss
            },
            None => Ok(None),
        }
    }

    /// Evict every indicator entry whose source lineage differs from `current_lineage`, returning the
    /// number removed. This is the eviction half of "cache invalidation tied to source lineage".
    ///
    /// # Errors
    /// [`StorageError`] on an LMDB failure.
    pub fn invalidate_stale_indicators(
        &self,
        current_lineage: &str,
    ) -> Result<usize, StorageError> {
        let mut stale: Vec<Vec<u8>> = Vec::new();
        {
            let rtxn = self.env.read_txn()?;
            for item in self.indicators.iter(&rtxn)? {
                let (k, v) = item?;
                let fresh = matches!(decode_cache_value(v), Some((lineage, _)) if lineage == current_lineage);
                if !fresh {
                    stale.push(k.to_vec());
                }
            }
        }
        let mut wtxn = self.env.write_txn()?;
        for k in &stale {
            self.indicators.delete(&mut wtxn, k.as_slice())?;
        }
        wtxn.commit()?;
        Ok(stale.len())
    }

    // ---- multi-resolution (reconstructed) bars ----------------------------------------------

    /// Cache reconstructed bars for `instrument`, tagged with their `source_lineage`.
    ///
    /// # Errors
    /// [`StorageError`] on an LMDB failure.
    pub fn put_recon_bars(
        &self,
        instrument: &InstrumentId,
        source_lineage: &str,
        bars: &[Bar],
    ) -> Result<(), StorageError> {
        let mut wtxn = self.env.write_txn()?;
        for bar in bars {
            let key = bar_key(instrument, bar.resolution(), bar.open_time());
            let value = ReconBar {
                source_lineage: source_lineage.to_owned(),
                bar: bar.clone(),
            };
            self.recon_bars.put(&mut wtxn, &key, &value)?;
        }
        wtxn.commit()?;
        Ok(())
    }

    /// Fetch a reconstructed bar **iff** it was derived from `current_lineage` (else `None`).
    ///
    /// # Errors
    /// [`StorageError`] on an LMDB failure.
    pub fn get_recon_bar(
        &self,
        instrument: &InstrumentId,
        resolution: Resolution,
        time: Timestamp,
        current_lineage: &str,
    ) -> Result<Option<Bar>, StorageError> {
        let rtxn = self.env.read_txn()?;
        match self
            .recon_bars
            .get(&rtxn, &bar_key(instrument, resolution, time))?
        {
            Some(rb) if rb.source_lineage == current_lineage => Ok(Some(rb.bar)),
            _ => Ok(None),
        }
    }

    /// Scan reconstructed bars for `instrument`+`resolution` over `[from, to)`, chronological.
    ///
    /// # Errors
    /// [`StorageError`] on an LMDB failure.
    pub fn scan_recon_bars(
        &self,
        instrument: &InstrumentId,
        resolution: Resolution,
        from: Timestamp,
        to: Timestamp,
    ) -> Result<Vec<Bar>, StorageError> {
        let rtxn = self.env.read_txn()?;
        let prefix = bar_prefix(instrument, resolution);
        let mut out = Vec::new();
        for item in self.recon_bars.prefix_iter(&rtxn, prefix.as_slice())? {
            let (key, rb) = item?;
            let t = time_from_key(key).millis();
            if t >= to.millis() {
                break;
            }
            if t >= from.millis() {
                out.push(rb.bar);
            }
        }
        Ok(out)
    }
}

/// Value layout: `u32(len lineage) ‖ lineage ‖ state_bytes`. Stores raw state bytes (not JSON) so a
/// cached state is returned byte-identical to what was put.
fn encode_cache_value(lineage: &str, state: &[u8]) -> Vec<u8> {
    let lb = lineage.as_bytes();
    let mut v = Vec::with_capacity(4 + lb.len() + state.len());
    v.extend_from_slice(&(lb.len() as u32).to_be_bytes());
    v.extend_from_slice(lb);
    v.extend_from_slice(state);
    v
}

/// Inverse of [`encode_cache_value`]; `None` if the bytes are malformed.
fn decode_cache_value(bytes: &[u8]) -> Option<(&str, &[u8])> {
    let len_buf: [u8; 4] = bytes.get(0..4)?.try_into().ok()?;
    let len = u32::from_be_bytes(len_buf) as usize;
    let rest = bytes.get(4..)?;
    let lineage = std::str::from_utf8(rest.get(..len)?).ok()?;
    let state = rest.get(len..)?;
    Some((lineage, state))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_value_round_trips_including_empty_and_binary() {
        for (lin, state) in [
            ("abc", &b"hello"[..]),
            ("", &b""[..]),
            ("lineage-1", &[0u8, 1, 2, 255, 0, 7][..]), // binary incl. NUL
        ] {
            let enc = encode_cache_value(lin, state);
            let (dl, ds) = decode_cache_value(&enc).unwrap();
            assert_eq!(dl, lin);
            assert_eq!(ds, state);
        }
    }

    #[test]
    fn decode_rejects_truncated_value() {
        assert!(decode_cache_value(&[0, 0]).is_none()); // < 4 length bytes
        assert!(decode_cache_value(&[0, 0, 0, 9, b'x']).is_none()); // claims 9 lineage bytes, has 1
    }
}
