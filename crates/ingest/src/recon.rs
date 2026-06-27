//! Cache reconstructed multi-resolution bars into the synthetic LMDB store (QE-106).
//!
//! The reconstruction *logic* is the storage-free [`qe_signal::reconstruct`] (shared with the
//! runtime streaming path); this bridge is the batch-only step that rolls the base (5m) bars up to
//! the configured coarser tiers and writes them to [`SyntheticStore`], tagged with the source
//! lineage so stale tiers are detected and not served.

use qe_domain::{Bar, InstrumentId, Resolution};
use qe_signal::{reconstruct_tiers, ReconError};
use qe_storage::{StorageError, SyntheticStore};
use thiserror::Error;

/// Errors from reconstructing + caching multi-resolution bars.
#[derive(Debug, Error)]
pub enum ReconCacheError {
    /// The reconstruction itself failed (bad tier or wrong-resolution input).
    #[error("reconstruction error: {0}")]
    Recon(#[from] ReconError),
    /// The synthetic store write failed.
    #[error("synthetic store error: {0}")]
    Storage(#[from] StorageError),
}

/// Reconstruct `base_bars` (resolution `base`, e.g. 5m) into every tier in `tiers` (e.g. 30m, 4h)
/// and cache the result into `store` for `instrument`, tagged with `source_lineage`.
///
/// Returns the number of reconstructed bars cached. The reconstruction is deterministic and uses
/// the same fold as the runtime streaming path (QE-206), so the cached tiers match what streaming
/// would produce.
///
/// # Errors
/// [`ReconCacheError`] if a tier is an invalid target, an input bar has the wrong resolution, or the
/// store write fails.
pub fn cache_reconstructed_tiers(
    store: &SyntheticStore,
    instrument: &InstrumentId,
    source_lineage: &str,
    base_bars: &[Bar],
    base: Resolution,
    tiers: &[Resolution],
) -> Result<usize, ReconCacheError> {
    let reconstructed = reconstruct_tiers(base_bars, base, tiers)?;
    store.put_recon_bars(instrument, source_lineage, &reconstructed)?;
    Ok(reconstructed.len())
}
