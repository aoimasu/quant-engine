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
//!
//! QE-102 adds the [`rest`]/[`backfill`] month-to-date client; QE-103 the
//! [`integrity`]/[`fill`]/[`coverage`]/[`reconcile`]/[`quality`] validation layer. QE-104 adds
//! **fusion**: [`canonical`] (the fixed series set), [`derive`] (VWAP, adjustments, spread),
//! [`coalesce`] (daily→monthly), and [`fuse`] (temporal alignment onto the base grid →
//! [`FusedCorpus`], byte-reproducible). Arrow record-batch/IPC output (`corpus_to_ipc`) is behind
//! the default-off `arrow` feature, so CI's default build stays dependency-light.

#[cfg(feature = "arrow")]
pub mod arrow;
pub mod backfill;
pub mod binance;
pub mod cache;
pub mod canonical;
pub mod checksum;
pub mod coalesce;
pub mod coverage;
pub mod derive;
pub mod downloader;
pub mod drift;
pub mod features;
pub mod fetch_all;
pub mod fetcher;
pub mod fill;
pub mod fuse;
pub mod integrity;
pub mod liquidity;
pub mod persist;
pub mod plan;
pub mod quality;
pub mod recon;
pub mod reconcile;
pub mod rest;
pub mod source;

pub use backfill::{
    BackfillRequest, BackfillResult, Backfiller, RealSleeper, RetryPolicy, Sleeper,
};
pub use binance::{
    closed_funding, closed_klines, decode_funding, decode_funding_row, decode_kline_row,
    decode_klines, plan_missing, BinanceHistorical, CalibrationSource, IngestedWindow,
    WindowRequest, FUNDING_INTERVAL_MS,
};
pub use cache::RawCache;
pub use canonical::CanonicalSeries;
pub use coalesce::coalesce_bars;
pub use coverage::{coverage, flag_short_history, Coverage, ShortCoverage};
pub use derive::{adjust_bar, spread_to_underlier, typical_price, vwap, Adjustment};
pub use downloader::{Downloader, FileOutcome, SyncReport};
pub use drift::{csv_header, detect_drift, DriftStatus, SchemaRegistry};
pub use features::{
    assemble_and_cache_features, read_cached_feature, FeatureCacheError, FEATURE_VECTOR_ID,
};
pub use fetch_all::{resolve_fetch_all, FetchAllResolution};
pub use fetcher::{FetchError, Fetcher};
pub use fill::{plan_fill, FillPlan, FilledPoint, Hole};
pub use fuse::{align_onto_grid, fuse, Cell, FusedColumn, FusedCorpus, FusionInput, Grid};
pub use integrity::{check_series, Gap, SeriesIntegrity};
pub use liquidity::{
    screen_liquidity, tradable_only, LiquidityInput, LiquidityVerdict, ScreenedInstrument,
    DEFAULT_MIN_ADV_USD,
};
pub use persist::{
    fused_bars, persist_fused, FusedMarket, PersistError, PersistReport, PersistStatus,
};
pub use plan::enumerate_targets;
pub use quality::{DataQualityReport, HardViolationPolicy, SeriesQuality, Violation};
pub use recon::{cache_reconstructed_tiers, ReconCacheError};
pub use reconcile::{diff_overlap, Divergence, Tolerance};
pub use rest::{
    parse_klines_json, PageRequest, RestEndpoint, RestError, RestSource, TimedRow,
    DEFAULT_REST_BASE,
};
pub use source::{DataKind, Date, DumpFile, Period, YearMonth, DEFAULT_BASE_URL};

#[cfg(feature = "http")]
pub use fetcher::HttpFetcher;
#[cfg(feature = "http")]
pub use rest::HttpRestSource;

#[cfg(feature = "arrow")]
pub use arrow::{corpus_schema, corpus_to_ipc, corpus_to_record_batch};

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
