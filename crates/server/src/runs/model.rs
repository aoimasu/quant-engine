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

/// The run-param **wire DTOs** now live in the dependency-free `qe-run-protocol` leaf crate (QE-406) so
/// there is one definition shared across the CLI ↔ server ↔ SPA boundary. Re-exported here so the
/// existing `super::model::{BacktestParams, TrainParams}` import paths (and the `meta.params` wire
/// shape they define) are unchanged. Their `#[serde(default)]` leniency and the CLI-mirroring defaults
/// (`taker_fee_bps` / `slippage_model`) are preserved verbatim on the shared types.
pub use qe_run_protocol::{BacktestParams, EvolveParams, TrainParams};

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
    /// A QE-452 offline GP indicator-**evolution** campaign — produces a sealed **formula pool**, never a
    /// vintage (§13.3).
    Evolve(EvolveParams),
}

impl RunSpec {
    /// The canonical run-type string recorded in `meta.json` / `index.json`.
    pub fn run_type(&self) -> &'static str {
        match self {
            RunSpec::Backtest(_) => "backtest",
            RunSpec::Train(_) => "train",
            RunSpec::Evolve(_) => "evolve",
        }
    }

    /// The params as a `serde_json::Value` for persistence in `meta.params` (backtest keeps its exact
    /// QE-255 byte-shape). Serialization of these plain structs cannot fail.
    pub fn params_value(&self) -> Value {
        match self {
            RunSpec::Backtest(p) => serde_json::to_value(p),
            RunSpec::Train(p) => serde_json::to_value(p),
            RunSpec::Evolve(p) => serde_json::to_value(p),
        }
        .unwrap_or(Value::Null)
    }

    /// The human discovery label for `index.json` (backtest: the vintage id; train/evolve: the window).
    pub fn label(&self) -> String {
        match self {
            RunSpec::Backtest(p) => p.vintage.clone(),
            RunSpec::Train(p) => format!("train {}→{}", p.start, p.end),
            RunSpec::Evolve(p) => format!("evolve {}→{} ({})", p.start, p.end, p.mode.as_str()),
        }
    }

    /// Whether a run of this kind ever writes a vintage. **`evolve` never does** (§13.3): it produces a
    /// pool artefact only. This is the load-bearing lifecycle-separation predicate the supervisor asserts
    /// against the terminal `done` line.
    pub fn writes_vintage(&self) -> bool {
        matches!(self, RunSpec::Train(_))
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
    /// QE-454 Phase B (design §13.5 "displayed = enforced = evidenced"): the **uncensored PBO** the GP/evolve
    /// monitor surfaces. **Absent-by-default** (`skip_serializing_if`) — the normal (non-evolve) train run
    /// leaves it `None`, so `GateSnapshot`/`TrainProgress`/`meta.json` serialise byte-identically.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uncensored_pbo: Option<f64>,
    /// QE-454 Phase B: the uncensored Sharpe-dispersion population size. Absent-by-default (normal path `None`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variance_trials: Option<u64>,
    /// QE-454 Phase B: distinct-canonical formulas evaluated (QE-439 GP trial basis). Absent-by-default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distinct_evaluations: Option<u64>,
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
    /// The sealed vintage id from the terminal `done` (the deep-link target). Set only by a `train` run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vintage: Option<String>,
    /// The sealed **formula-pool** id from the terminal `done` (QE-452 `evolve` run). Set only by an
    /// `evolve` run — **mutually exclusive** with `vintage` (§13.3). Absent (`None`) for train/backtest,
    /// so their `meta.json` shape is unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool: Option<String>,
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
