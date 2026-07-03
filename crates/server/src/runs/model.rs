//! Run-store data model (QE-255): the on-disk `meta.json` / `index.json` shapes, the API request
//! body, and the subprocess progress line. Serde field names are the wire + file contract.

use serde::{Deserialize, Serialize};

/// Lifecycle status of a run. `queued → running → succeeded | failed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    /// Created, waiting for a worker-pool slot.
    Queued,
    /// A subprocess is executing.
    Running,
    /// The subprocess emitted `done` and exited 0.
    Succeeded,
    /// The subprocess exited non-zero (or could not be spawned/tailed).
    Failed,
}

/// The latest progress update tailed from the subprocess stdout (`§5.3`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    /// Completion percentage `0..=100`.
    pub pct: u8,
    /// Coarse stage label (`load|scan|features|simulate|report`).
    pub stage: String,
    /// Human-readable status line.
    pub msg: String,
}

/// Default taker fee (bps) — mirrors the `qe-cli backtest` default so an omitted field behaves the
/// same as the CLI's own default.
fn default_taker_fee_bps() -> f64 {
    2.0
}

/// Default slippage-model label — mirrors the `qe-cli backtest` default.
fn default_slippage_model() -> String {
    "square-root-impact".to_owned()
}

/// Backtest parameters — the `params` object of a create-run request, persisted verbatim in
/// `meta.json` and mapped 1:1 onto the `qe-cli backtest` flags.
///
/// **Every** field is `#[serde(default)]` so the body parses **leniently**: a missing required field
/// deserialises to an empty value rather than a serde reject (which axum would surface as `422`). All
/// required-ness is then enforced in one place (`manager::validate`), which returns a uniform `400`
/// with a clear message for any missing/invalid param (nit 2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BacktestParams {
    /// Vintage id to backtest (required; `--vintage`).
    #[serde(default)]
    pub vintage: String,
    /// Optional single-chromosome selector (`--strategy`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy: Option<String>,
    /// Inclusive window start `YYYY-MM-DD` (required; `--start`).
    #[serde(default)]
    pub start: String,
    /// Exclusive window end `YYYY-MM-DD` (required; `--end`).
    #[serde(default)]
    pub end: String,
    /// Bar resolution (required; `--resolution`).
    #[serde(default)]
    pub resolution: String,
    /// Instrument symbols (`--universe`, repeated). Must be non-empty (the job needs ≥1 instrument).
    #[serde(default)]
    pub universe: Vec<String>,
    /// Taker fee, basis points (`--taker-fee-bps`).
    #[serde(default = "default_taker_fee_bps")]
    pub taker_fee_bps: f64,
    /// Slippage-model label (`--slippage-model`).
    #[serde(default = "default_slippage_model")]
    pub slippage_model: String,
}

impl Default for BacktestParams {
    fn default() -> Self {
        Self {
            vintage: String::new(),
            strategy: None,
            start: String::new(),
            end: String::new(),
            resolution: String::new(),
            universe: Vec::new(),
            taker_fee_bps: default_taker_fee_bps(),
            slippage_model: default_slippage_model(),
        }
    }
}

/// Default run type when omitted from a create request.
fn default_run_type() -> String {
    "backtest".to_owned()
}

/// `POST /api/runs` body: `{ "type": "backtest", "params": { … } }`.
///
/// Both fields are `#[serde(default)]` so the body parses leniently and every required-ness check is
/// enforced uniformly in `manager::validate` as a `400` (never a serde `422`).
#[derive(Debug, Clone, Deserialize)]
pub struct CreateRunRequest {
    /// Run type — only `backtest` is supported in v1.
    #[serde(rename = "type", default = "default_run_type")]
    pub run_type: String,
    /// Backtest parameters.
    #[serde(default)]
    pub params: BacktestParams,
}

/// A run's `meta.json` — the **authoritative** status + progress record (§6.1 / §8.2).
///
/// Timestamps are wall-clock (`created_ms` etc.): `meta.json` is operational state, not the
/// deterministic `result.json` (which the CLI produces wall-clock-free), so this does not breach the
/// determinism boundary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunMeta {
    /// Opaque run id (uuid v4).
    pub id: String,
    /// Run type (`backtest`).
    #[serde(rename = "type")]
    pub run_type: String,
    /// Current lifecycle status.
    pub status: RunStatus,
    /// The parameters the run was created with.
    pub params: BacktestParams,
    /// Latest tailed progress.
    pub progress: Progress,
    /// Creation time (epoch-ms).
    pub created_ms: u64,
    /// Time the run transitioned to `running` (epoch-ms), once started.
    pub started_ms: Option<u64>,
    /// Time the run finished (epoch-ms), once terminal.
    pub finished_ms: Option<u64>,
    /// Child process exit code, once finished.
    pub exit: Option<i32>,
    /// Error tail (captured stderr) on failure.
    pub error: Option<String>,
    /// Artefacts written into the run dir (e.g. `result.json`) once available.
    pub artifacts: Vec<String>,
}

/// One entry in `index.json` — immutable per-run discovery/order fields only. Status/progress are
/// **never** duplicated here (they live solely in `meta.json`), so the index can never diverge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexEntry {
    /// Run id.
    pub id: String,
    /// Run type.
    #[serde(rename = "type")]
    pub run_type: String,
    /// Creation time (epoch-ms).
    pub created_ms: u64,
    /// Human label (v1: the vintage id).
    pub label: String,
}
