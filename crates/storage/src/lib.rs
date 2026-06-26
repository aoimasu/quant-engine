//! qe-storage — embedded storage for the quant engine.
//!
//! QE-010 fills this with [`MarketStore`], an LMDB-backed store for the fused market corpus:
//! OHLCVT bars, funding rates, premium/spread-to-underlier, and futures metrics, keyed by
//! instrument (+ resolution for bars) + time, with chronological range scans and a versioned schema.
//! (Synthetic/indicator cache is QE-011.)

pub mod key;
pub mod records;
pub mod store;

pub use records::{FuturesMetrics, PremiumSample};
pub use store::{MarketStore, DEFAULT_MAP_SIZE};

use thiserror::Error;

/// On-disk schema version. Bump when the key layout or record shape changes incompatibly.
pub const SCHEMA_VERSION: u32 = 1;

/// Errors from the storage layer.
#[derive(Debug, Error)]
pub enum StorageError {
    /// Filesystem error creating or accessing the store directory.
    #[error("storage io error: {0}")]
    Io(#[from] std::io::Error),

    /// An error from the underlying LMDB engine.
    #[error("lmdb error: {0}")]
    Lmdb(#[from] heed::Error),

    /// The store's recorded schema version does not match this build's [`SCHEMA_VERSION`].
    #[error("schema version mismatch: store has {found}, code expects {expected}")]
    SchemaMismatch {
        /// The version this build expects.
        expected: u32,
        /// The version found on disk.
        found: u32,
    },

    /// The schema-version record is missing or unparseable.
    #[error("schema version record is corrupt: {0:?}")]
    SchemaCorrupt(String),
}
