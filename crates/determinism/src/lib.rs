//! QE-006 — determinism & reproducibility primitives.
//!
//! Auditable, reproducible vintages rest on three pieces:
//! - [`rng`] — a seedable, portable RNG with per-task seed derivation, so parallel stages are
//!   byte-identical regardless of core/thread count;
//! - [`harness`] — re-run a stage twice and assert byte-identical artefacts;
//! - [`lineage`] — a resolvable lineage record (config hash + input snapshot id + code commit +
//!   seeds) attached to every produced artefact.
//!
//! The master seed comes from `qe_config::Config`'s `determinism.seed`, and the config hash from
//! its [`content_hash`](qe_config::Config::content_hash) — the two QE-002 inputs to a lineage.

pub mod harness;
pub mod lineage;
pub mod rng;

pub use harness::{is_reproducible, reproduce, ReproError};
pub use lineage::{Artifact, HasLineage, Lineage, LineageError};
pub use rng::{derive_seed, seed_rng, task_rng, DetRng};
