//! Cache assembled feature vectors into the synthetic LMDB store (QE-108).
//!
//! Assembly is the storage-free [`qe_signal::FeatureAssembler`] (batch/streaming-identical); this
//! bridge writes each **complete** per-bar vector (every indicator warm) as one opaque blob into the
//! synthetic indicator-state cache, tagged with the source lineage so stale vintages are detected.

use qe_domain::{InstrumentId, Resolution, Timestamp};
use qe_signal::{assemble_batch, CatalogueConfig, FeatureSchema, FeatureVector, Sample};
use qe_storage::{IndicatorKey, StorageError, SyntheticStore};
use thiserror::Error;

/// Reserved `indicator_id` under which a whole feature vector is cached in the indicator-state
/// cache. No catalogue indicator uses this id, so it cannot collide.
pub const FEATURE_VECTOR_ID: &str = "feature_vector";

/// Errors from assembling + caching feature vectors.
#[derive(Debug, Error)]
pub enum FeatureCacheError {
    /// The synthetic store write failed.
    #[error("synthetic store error: {0}")]
    Storage(#[from] StorageError),
}

/// Assemble per-bar feature vectors from `samples` and cache every **complete** one into `store`
/// for `instrument`+`resolution`, tagged with `source_lineage`.
///
/// Returns the number of (complete) vectors cached. Incomplete (warmup) vectors are skipped — only
/// fully-populated rows are the ones WFO/DE consume.
///
/// # Errors
/// [`FeatureCacheError`] if a store write fails.
pub fn assemble_and_cache_features(
    store: &SyntheticStore,
    instrument: &InstrumentId,
    resolution: Resolution,
    source_lineage: &str,
    cfg: &CatalogueConfig,
    samples: &[Sample],
) -> Result<usize, FeatureCacheError> {
    let schema = FeatureSchema::from_catalogue(cfg);
    let lookback = schema.max_lookback();
    let vectors = assemble_batch(cfg, samples);
    let mut cached = 0;
    for v in vectors.iter().filter(|v| v.is_complete()) {
        let key = IndicatorKey {
            instrument,
            resolution,
            indicator_id: FEATURE_VECTOR_ID,
            lookback: lookback as u32,
            time: Timestamp::from_millis(v.time_ms),
        };
        store.put_indicator_state(&key, source_lineage, &v.to_bytes(&schema))?;
        cached += 1;
    }
    Ok(cached)
}

/// Read back a cached feature vector for `instrument`+`resolution` at `time`, if present and derived
/// from `current_lineage` (else `None` — absent or stale). `width` is the schema length.
///
/// # Errors
/// [`FeatureCacheError`] if the store read fails.
pub fn read_cached_feature(
    store: &SyntheticStore,
    instrument: &InstrumentId,
    resolution: Resolution,
    time: Timestamp,
    current_lineage: &str,
    cfg: &CatalogueConfig,
) -> Result<Option<FeatureVector>, FeatureCacheError> {
    let schema = FeatureSchema::from_catalogue(cfg);
    let key = IndicatorKey {
        instrument,
        resolution,
        indicator_id: FEATURE_VECTOR_ID,
        lookback: schema.max_lookback() as u32,
        time,
    };
    let bytes = store.get_indicator_state(&key, current_lineage)?;
    Ok(bytes.and_then(|b| FeatureVector::from_bytes(&b, &schema)))
}
