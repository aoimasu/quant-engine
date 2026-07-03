//! Run-store data model (QE-255): the on-disk `meta.json` / `index.json` shapes, the API request
//! body, and the subprocess progress line. Serde field names are the wire + file contract.
//!
//! QE-261 extends this to a second run type — `train` — without disturbing the `backtest` wire/file
//! shapes: `params` is stored as an opaque `serde_json::Value` (so a backtest's `meta.params` is
//! byte-identical to QE-255), while an internal, non-serialized [`RunSpec`] carries the typed params
//! that drive subprocess spawning. Train runs additionally expose the QE-260 rich progress
//! (`gen`/`ensemble`/`gate` + the sealed vintage id) under the optional [`RunMeta::train`] field.

use serde::{Deserialize, Serialize};
use serde_json::Value;

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

/// Training parameters — the `params` object of a `type:"train"` create-run request (QE-261),
/// persisted verbatim in `meta.json` and mapped onto the `qe-cli train` flags.
///
/// Like [`BacktestParams`], every field is `#[serde(default)]` so the body parses **leniently**; the
/// required-ness of the window (`start`/`end`/`resolution`) is enforced in one place
/// (`manager::validate`) as a uniform `400`. The budget knobs are optional — `qe train` supplies its
/// own defaults when a flag is omitted. The **instrument/universe** is not a flag: `qe train` resolves
/// it from the config file (`--config`), so it is deliberately absent here (the QE-260 CLI is unchanged).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TrainParams {
    /// Inclusive training-window start `YYYY-MM-DD` (required; `--start`).
    #[serde(default)]
    pub start: String,
    /// Exclusive training-window end `YYYY-MM-DD` (required; `--end`).
    #[serde(default)]
    pub end: String,
    /// Bar resolution (required; `--resolution`).
    #[serde(default)]
    pub resolution: String,
    /// Master search seed override (`--seed`); omitted ⇒ the config seed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    /// MAP-Elites search generations (`--generations`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generations: Option<usize>,
    /// Variation steps per direction per generation (`--population`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub population: Option<usize>,
    /// Final bars reserved as the untouched G1 holdout (`--holdout`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holdout: Option<usize>,
    /// Embargo bars purged between the train window and the holdout (`--embargo`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embargo: Option<usize>,
    /// Optional config-file path override (`--config`); omitted ⇒ the CLI default (`config.toml`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<String>,
    /// Optional operating profile override (`--profile`); omitted ⇒ the CLI default (`train`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
}

/// Typed, **non-serialized** run parameters that drive subprocess spawning. Built by `manager::create`
/// from the validated request; the spawner matches on it to build either the `backtest` or `train`
/// argv. The persisted `meta.params` is the `serde_json::Value` of the inner params (see
/// [`RunSpec::params_value`]), never this enum — so the backtest file/wire shape is unchanged.
#[derive(Debug, Clone, PartialEq)]
pub enum RunSpec {
    /// A QE-251/QE-255 backtest run.
    Backtest(BacktestParams),
    /// A QE-260/QE-261 training-search run.
    Train(TrainParams),
}

impl RunSpec {
    /// The canonical run-type string recorded in `meta.json` / `index.json`.
    pub fn run_type(&self) -> &'static str {
        match self {
            RunSpec::Backtest(_) => "backtest",
            RunSpec::Train(_) => "train",
        }
    }

    /// The params as a `serde_json::Value` for persistence in `meta.params` (backtest keeps its exact
    /// QE-255 byte-shape). Serialization of these plain structs cannot fail.
    pub fn params_value(&self) -> Value {
        match self {
            RunSpec::Backtest(p) => serde_json::to_value(p),
            RunSpec::Train(p) => serde_json::to_value(p),
        }
        .unwrap_or(Value::Null)
    }

    /// The human discovery label for `index.json` (backtest: the vintage id; train: the window).
    pub fn label(&self) -> String {
        match self {
            RunSpec::Backtest(p) => p.vintage.clone(),
            RunSpec::Train(p) => format!("train {}→{}", p.start, p.end),
        }
    }
}

/// Default run type when omitted from a create request.
fn default_run_type() -> String {
    "backtest".to_owned()
}

/// `POST /api/runs` body: `{ "type": "backtest" | "train", "params": { … } }`.
///
/// `params` is kept as an opaque `serde_json::Value` so the body parses **leniently** regardless of the
/// run type; `manager::create` then deserializes it into the typed params for the run type and enforces
/// every required-ness check uniformly as a `400` (never a serde `422`).
#[derive(Debug, Clone, Deserialize)]
pub struct CreateRunRequest {
    /// Run type — `backtest` (QE-255) or `train` (QE-261).
    #[serde(rename = "type", default = "default_run_type")]
    pub run_type: String,
    /// Run parameters, typed per `run_type` in `manager::create`.
    #[serde(default)]
    pub params: Value,
}

/// Latest MAP-Elites search-generation snapshot (QE-260 `gen` line). Float fields are `Option` because
/// `serde_json` renders a non-finite `f64` (e.g. `-inf` best-so-far before any accepted elite) as
/// `null`; a required `f64` would make the whole progress line fail to parse and be dropped.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GenSnapshot {
    /// The generation just completed (`1..=generations`).
    pub generation: usize,
    /// Total generations in the budget.
    pub generations: usize,
    /// Total occupied MAP-Elites cells across both directions.
    pub coverage: usize,
    /// Occupied cells in the Long archive.
    pub coverage_long: usize,
    /// Occupied cells in the Short archive.
    pub coverage_short: usize,
    /// Best archive fitness seen so far (`None` while it is still `-inf`).
    pub best_fitness: Option<f64>,
}

/// Latest ensemble-construction snapshot (QE-260 `ensemble` line).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnsembleSnapshot {
    /// Cross-validation folds the portfolio search scored over.
    pub folds: usize,
    /// Chromosomes selected into the ensemble.
    pub members: usize,
    /// The converged cross-validated robust-basin score (`None` if non-finite).
    pub score: Option<f64>,
}

/// The G1 gate verdict snapshot (QE-260/QE-134 `gate` line).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GateSnapshot {
    /// Whether the vintage cleared every G1 criterion.
    pub promoted: bool,
    /// The names of the criteria that failed (empty iff promoted).
    pub failed: Vec<String>,
    /// In-sample (train-window) net-of-cost Sharpe (`None` if non-finite).
    pub in_sample_sharpe: Option<f64>,
    /// Holdout (untouched OOS) net-of-cost Sharpe (`None` if non-finite).
    pub holdout_sharpe: Option<f64>,
    /// Deflated Sharpe Ratio the DSR criterion evaluated (`None` if non-finite).
    pub dsr: Option<f64>,
    /// White's Reality Check / SPA p-value (`None` if non-finite).
    pub spa_pvalue: Option<f64>,
    /// Effective number of trials the DSR deflated against.
    pub n_trials: usize,
}

/// The rich training progress a `train` run exposes for polling (QE-261). Holds the **latest** of each
/// QE-260 progress kind plus the sealed vintage id from the terminal `done`. Absent (`None`) for
/// backtest runs, so a backtest's `meta.json` is unchanged.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TrainProgress {
    /// Latest search-generation snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<GenSnapshot>,
    /// Ensemble-construction snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ensemble: Option<EnsembleSnapshot>,
    /// G1 gate verdict snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate: Option<GateSnapshot>,
    /// The sealed vintage id from the terminal `done` (the deep-link target).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vintage: Option<String>,
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
    /// Run type (`backtest` or `train`).
    #[serde(rename = "type")]
    pub run_type: String,
    /// Current lifecycle status.
    pub status: RunStatus,
    /// The parameters the run was created with (typed per `type` at create time; stored opaquely).
    pub params: Value,
    /// Latest tailed coarse progress (pct/stage/msg — advanced by every progress kind).
    pub progress: Progress,
    /// Rich training progress (QE-261) — present only on `train` runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub train: Option<TrainProgress>,
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
