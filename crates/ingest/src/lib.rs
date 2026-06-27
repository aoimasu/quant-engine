//! qe-ingest — external-source ingestion.
//!
//! QE-101: the Binance public-dumps downloader. It fetches the bulk long-range history from
//! `data.binance.vision` (klines, funding, premium-index, `/futures/data` metrics) for the
//! configured point-in-time universe, **checksum-verified, idempotent, and resumable**, caching raw
//! files locally and flagging cross-month schema drift.
//!
//! - [`source`] — the `data.binance.vision` layout ([`DumpFile`], [`DataKind`], [`Period`]).
//! - [`plan`] — point-in-time target enumeration over the [`qe_config::Universe`].
//! - [`fetcher`] — the byte-transport port ([`Fetcher`]); the real `HttpFetcher` is behind `http`.
//! - [`cache`] — the local raw-file cache with digest sidecars.
//! - [`downloader`] — the idempotent, resumable, checksum-verifying [`Downloader`].
//! - [`checksum`] / [`drift`] — SHA-256 verification and CSV schema-drift detection.

pub mod backfill;
pub mod cache;
pub mod checksum;
pub mod downloader;
pub mod drift;
pub mod fetcher;
pub mod plan;
pub mod rest;
pub mod source;

pub use backfill::{BackfillRequest, BackfillResult, Backfiller, RetryPolicy};
pub use cache::RawCache;
pub use downloader::{Downloader, FileOutcome, SyncReport};
pub use drift::{csv_header, detect_drift, DriftStatus, SchemaRegistry};
pub use fetcher::{FetchError, Fetcher};
pub use plan::enumerate_targets;
pub use rest::{
    parse_klines_json, PageRequest, RestEndpoint, RestError, RestSource, TimedRow,
    DEFAULT_REST_BASE,
};
pub use source::{DataKind, Date, DumpFile, Period, YearMonth, DEFAULT_BASE_URL};

#[cfg(feature = "http")]
pub use fetcher::HttpFetcher;
#[cfg(feature = "http")]
pub use rest::HttpRestSource;

use thiserror::Error;

/// Errors from the ingestion layer.
///
/// A 404 is **not** an error — a missing period is reported as [`downloader::FileOutcome::Missing`].
#[derive(Debug, Error)]
pub enum IngestError {
    /// A transport-level failure fetching a URL.
    #[error("transport error fetching {url}: {message}")]
    Transport {
        /// The URL that failed.
        url: String,
        /// The cause.
        message: String,
    },

    /// A downloaded file's bytes did not match its `.CHECKSUM` after a re-fetch.
    #[error("checksum mismatch for {0} (re-fetched, still corrupt)")]
    ChecksumMismatch(String),

    /// A `.CHECKSUM` sidecar held no parseable digest.
    #[error("unparseable checksum sidecar: {0}")]
    ChecksumUnparseable(String),

    /// A ZIP archive could not be read (schema-drift header extraction).
    #[error("archive error: {0}")]
    Archive(String),

    /// A REST backfill page failed fatally or exhausted the retry policy (QE-102).
    #[error("rest backfill error: {0}")]
    Rest(String),

    /// A filesystem error in the raw cache.
    #[error("io error at {path}: {source}")]
    Io {
        /// The path being operated on.
        path: String,
        /// The underlying error.
        source: std::io::Error,
    },
}
