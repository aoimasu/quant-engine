//! Run store + run lifecycle + subprocess supervision (QE-255).
//!
//! - [`store`] — the file-based run store (ADR D4b): `meta.json` / `index.json` / `result.json` /
//!   `stdout.log` under `<data_dir>/runs`.
//! - [`spawn`] — the subprocess spawn seam (ADR D4c): [`CliJobSpawner`] runs `qe-cli backtest`;
//!   tests inject a fake job via the same seam.
//! - [`manager`] — [`RunManager`]: validation, the bounded worker pool, and the supervisor loop.
//! - [`api`] — the `/api/runs*` axum routes.

pub mod api;
pub mod manager;
pub mod model;
pub mod spawn;
pub mod store;

pub use manager::{CreateError, RunManager};
pub use model::{BacktestParams, CreateRunRequest, RunMeta, RunStatus};
pub use spawn::{resolve_cli_bin, CliJobSpawner, JobSpawner};
