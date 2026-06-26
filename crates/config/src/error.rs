//! Configuration error type.
//!
//! Kept local to `qe-config` for now; the shared error model (QE-004) may later re-home or
//! wrap these variants.

use thiserror::Error;

/// Errors raised while loading, validating, or hashing configuration.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// The config could not be read or parsed from its sources (file/env).
    #[error("failed to load config: {0}")]
    Load(String),

    /// A specific field failed validation. `field` is a dotted path (e.g. `bars.base`).
    #[error("invalid config at `{field}`: {message}")]
    Invalid {
        /// Dotted path to the offending field.
        field: String,
        /// Human-readable explanation.
        message: String,
    },

    /// The resolved config could not be serialised for hashing.
    #[error("failed to serialise config for hashing: {0}")]
    Serialize(String),
}
