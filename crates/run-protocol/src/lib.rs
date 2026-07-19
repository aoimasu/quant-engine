//! qe-run-protocol (QE-406) — the single source of truth for the CLI ↔ server ↔ SPA **run protocol**.
//!
//! The admin server spawns the deterministic `qe-cli` pipelines as subprocesses (ADR D4c). Each job
//! writes artefacts into a `--run-dir` and streams JSON-line **progress** on stdout: a sequence of
//! `progress`/`gen`/`ensemble`/`gate` lines followed by exactly one terminal `done` or `error` line.
//! The server tails that stream and folds it into `meta.json`; the SPA renders `meta.json`.
//!
//! Historically this contract was defined **three** times — emit (`qe-cli`), parse (`qe-server`), and
//! the SPA — with no shared schema and no version. This crate holds the wire types **once**:
//!
//! * [`ProgressLine`] — the progress-line enum, used for both `Serialize` (cli emit) and `Deserialize`
//!   (server parse). Float fields are `Option<f64>` so the server's tolerance of non-finite floats is
//!   preserved (see the type docs); the emitted bytes are unchanged.
//! * [`emit_progress`] / [`emit_done`] / [`emit_train_done`] / [`emit_error`] — the byte-exact writers.
//! * [`BacktestParams`] / [`TrainParams`] — the run-param **wire DTOs** (the `params` object of a
//!   create-run request, persisted verbatim into `meta.params`).
//! * [`PROTOCOL_VERSION`] — the contract version the CLI stamps on the terminal `done` line and the
//!   server checks.
//!
//! **Firewall.** This crate depends on `serde`/`serde_json` only — no `qe-*` crate — so `qe-server`
//! depending on it introduces no forbidden edge (QE-132). The SPA mirror (`web/src/api/runs.ts`) is
//! hand-kept in lockstep with these types.

use std::io::{self, Write};

use serde::{Deserialize, Serialize};

/// The run-protocol wire version. The CLI stamps it on the terminal [`ProgressLine::Done`] line and the
/// server checks it (logging a warning on mismatch — see `qe_server::runs::manager`). Bump this on any
/// backward-incompatible change to the wire shapes below.
pub const PROTOCOL_VERSION: u32 = 3;

/// The `protocol_version` a terminal `done` line that predates QE-406 (or any line that omits the
/// field) deserializes to — distinct from every real [`PROTOCOL_VERSION`] so the server can detect and
/// warn on it without failing to parse the line.
const LEGACY_PROTOCOL_VERSION: u32 = 0;

/// The default for a missing `protocol_version` on deserialize (a legacy/omitted field).
fn legacy_protocol_version() -> u32 {
    LEGACY_PROTOCOL_VERSION
}

/// One JSON-line progress record on stdout. The stream is a sequence of progress lines followed by
/// exactly one terminal `done` or `error` line (see [`emit_progress`], [`emit_done`], [`emit_error`]).
///
/// This single type is **both** serialized (the `qe-cli` emit side) and deserialized (the `qe-server`
/// parse side). Its float fields are `Option<f64>` so the parse side tolerates non-finite floats:
/// `serde_json` renders a non-finite `f64` (e.g. a `-inf` best-so-far before any accepted elite) as
/// JSON `null` on **serialize**, and a required `f64` would fail to **deserialize** that `null` and
/// drop the whole line. The emit side wraps its finite values in `Some(..)`, which serialize to the
/// same numbers, so the on-wire bytes are unchanged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ProgressLine {
    /// An intermediate progress update.
    Progress {
        /// Completion percentage `0..=100`.
        pct: u8,
        /// Coarse stage label (`load|scan|features|simulate|report`).
        stage: String,
        /// Human-readable line.
        msg: String,
    },
    /// One MAP-Elites search generation (QE-260 train job). Carries the archive coverage and best-so-far
    /// fitness so the training monitor (QE-261) can render the generation → coverage → fitness trace.
    Gen {
        /// Completion percentage `0..=100`.
        pct: u8,
        /// Stage label (always `"search"`).
        stage: String,
        /// The generation just completed (`1..=generations`).
        generation: usize,
        /// Total generations in the budget.
        generations: usize,
        /// Total occupied MAP-Elites cells across both directions (`qe_wfo::regularise::coverage` sum).
        coverage: usize,
        /// Occupied cells in the Long archive.
        coverage_long: usize,
        /// Occupied cells in the Short archive.
        coverage_short: usize,
        /// Best archive fitness seen so far. `None` on the wire (`null`) while it is still non-finite
        /// (`-inf` before any accepted elite); the emit side passes `Some(fitness)`.
        #[serde(default)]
        best_fitness: Option<f64>,
    },
    /// The ensemble (portfolio) construction result (QE-260). Carries the CV fold count.
    Ensemble {
        /// Completion percentage `0..=100`.
        pct: u8,
        /// Stage label (always `"ensemble"`).
        stage: String,
        /// Cross-validation folds the portfolio search scored over.
        folds: usize,
        /// Number of chromosomes selected into the ensemble.
        members: usize,
        /// The converged cross-validated robust-basin score (`None`/`null` if non-finite).
        #[serde(default)]
        score: Option<f64>,
    },
    /// The G1 gate verdict (QE-260/QE-134). `promoted` is the pass/fail; `failed` names the blocking
    /// criteria (empty iff promoted).
    Gate {
        /// Completion percentage `0..=100`.
        pct: u8,
        /// Stage label (always `"gate"`).
        stage: String,
        /// Whether the vintage cleared every G1 criterion.
        promoted: bool,
        /// The names of the criteria that failed (empty iff promoted).
        #[serde(default)]
        failed: Vec<String>,
        /// In-sample (train-window) net-of-cost Sharpe (`None`/`null` if non-finite).
        #[serde(default)]
        in_sample_sharpe: Option<f64>,
        /// Holdout (untouched OOS) net-of-cost Sharpe (`None`/`null` if non-finite).
        #[serde(default)]
        holdout_sharpe: Option<f64>,
        /// Deflated Sharpe Ratio the DSR criterion evaluated (`None`/`null` if non-finite).
        #[serde(default)]
        dsr: Option<f64>,
        /// White's Reality Check / SPA p-value (`None`/`null` if non-finite).
        #[serde(default)]
        spa_pvalue: Option<f64>,
        /// Effective number of trials the DSR deflated against.
        n_trials: usize,
        /// QE-454 Phase B (design §13.5 "displayed = enforced = evidenced"): the **uncensored PBO** the
        /// GP/evolve monitor surfaces. **Absent-by-default** (`skip_serializing_if`) — the normal train
        /// path emits `None`, so its `gate` line + `meta.json` are byte-identical to pre-Phase-B.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        uncensored_pbo: Option<f64>,
        /// QE-454 Phase B: the uncensored Sharpe-dispersion population size (paired with `distinct_evaluations`
        /// so a censored population is visible). Absent-by-default (normal train path emits `None`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        variance_trials: Option<u64>,
        /// QE-454 Phase B: distinct-canonical formulas evaluated (the QE-439 GP trial basis). Absent-by-default
        /// (normal train path emits `None`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        distinct_evaluations: Option<u64>,
    },
    /// Terminal success: the artefact filename written into the run dir.
    Done {
        /// The result artefact name (`result.json`).
        result: String,
        /// The run-protocol version this line was emitted under ([`PROTOCOL_VERSION`]). A line that
        /// predates QE-406 (or omits the field) deserializes to [`LEGACY_PROTOCOL_VERSION`] (`0`).
        #[serde(default = "legacy_protocol_version")]
        protocol_version: u32,
        /// The sealed vintage id, when a terminal produces one (train job). Omitted for the backtest job
        /// so its `{"t":"done",…}` shape carries no `vintage` key.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        vintage: Option<String>,
        /// The sealed **formula-pool** id, when a terminal produces one (QE-452 `evolve` job). Omitted
        /// for the backtest/train jobs so their `done` shape is unchanged; **mutually exclusive** with
        /// `vintage` (an evolve run never writes a vintage — §13.3). A v1 `done` line (which predates
        /// QE-452) omits this and deserializes to `None`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pool: Option<String>,
        /// Loud marker that this terminal outcome produced **GENERATED / synthetic** data, not real
        /// market data (`qe ingest --synthetic`). **Absent-by-default** (`skip_serializing_if`): every
        /// existing terminal (backtest/train/evolve, and a real ingest) emits `false`, so its
        /// `{"t":"done",…}` wire is byte-identical to pre-synthetic — the [`PROTOCOL_VERSION`] is
        /// unchanged. When `true`, downstream tooling and humans can never mistake the store for real
        /// prices.
        #[serde(default, skip_serializing_if = "is_false")]
        synthetic: bool,
    },
    /// Terminal failure.
    Error {
        /// The failure message.
        msg: String,
    },
}

/// Write one `progress` line to `w`, newline-terminated. Deterministic (no timestamp).
///
/// # Errors
/// Propagates any write / serialisation failure.
pub fn emit_progress(w: &mut impl Write, pct: u8, stage: &str, msg: &str) -> io::Result<()> {
    let line = ProgressLine::Progress {
        pct,
        stage: stage.to_owned(),
        msg: msg.to_owned(),
    };
    write_line(w, &line)
}

/// Write the terminal `done` line (no vintage — the backtest/ingest form), stamped with the current
/// [`PROTOCOL_VERSION`].
///
/// # Errors
/// Propagates any write / serialisation failure.
pub fn emit_done(w: &mut impl Write, result: &str) -> io::Result<()> {
    write_line(
        w,
        &ProgressLine::Done {
            result: result.to_owned(),
            protocol_version: PROTOCOL_VERSION,
            vintage: None,
            pool: None,
            synthetic: false,
        },
    )
}

/// `#[serde(skip_serializing_if)]` predicate for the absent-by-default `synthetic` marker: a `false`
/// flag is omitted, so every non-synthetic terminal line's wire is byte-identical to pre-synthetic.
#[allow(clippy::trivially_copy_pass_by_ref)] // serde requires the `&bool` predicate signature
fn is_false(b: &bool) -> bool {
    !*b
}

/// Write the terminal `done` line for the `ingest` job, stamped with the current [`PROTOCOL_VERSION`].
///
/// `synthetic = true` sets the absent-by-default `synthetic` marker (`qe ingest --synthetic`), so the
/// terminal line loudly records that the store holds **GENERATED**, not real, market data. A real
/// ingest passes `false` and its wire is byte-identical to a backtest `done` line.
///
/// # Errors
/// Propagates any write / serialisation failure.
pub fn emit_ingest_done(w: &mut impl Write, result: &str, synthetic: bool) -> io::Result<()> {
    write_line(
        w,
        &ProgressLine::Done {
            result: result.to_owned(),
            protocol_version: PROTOCOL_VERSION,
            vintage: None,
            pool: None,
            synthetic,
        },
    )
}

/// Write the terminal `done` line naming the sealed `vintage` (the train form), stamped with the
/// current [`PROTOCOL_VERSION`].
///
/// # Errors
/// Propagates any write / serialisation failure.
pub fn emit_train_done(w: &mut impl Write, result: &str, vintage: &str) -> io::Result<()> {
    write_line(
        w,
        &ProgressLine::Done {
            result: result.to_owned(),
            protocol_version: PROTOCOL_VERSION,
            vintage: Some(vintage.to_owned()),
            pool: None,
            synthetic: false,
        },
    )
}

/// Write the terminal `done` line naming the sealed **formula `pool`** (the QE-452 `evolve` form),
/// stamped with the current [`PROTOCOL_VERSION`]. Emits `pool: Some(..)` and **never** a `vintage` — an
/// evolve run produces a pool artifact, never a vintage (§13.3).
///
/// # Errors
/// Propagates any write / serialisation failure.
pub fn emit_evolve_done(w: &mut impl Write, result: &str, pool: &str) -> io::Result<()> {
    write_line(
        w,
        &ProgressLine::Done {
            result: result.to_owned(),
            protocol_version: PROTOCOL_VERSION,
            vintage: None,
            pool: Some(pool.to_owned()),
            synthetic: false,
        },
    )
}

/// Write the terminal `error` line.
///
/// # Errors
/// Propagates any write / serialisation failure.
pub fn emit_error(w: &mut impl Write, msg: &str) -> io::Result<()> {
    write_line(
        w,
        &ProgressLine::Error {
            msg: msg.to_owned(),
        },
    )
}

fn write_line(w: &mut impl Write, line: &ProgressLine) -> io::Result<()> {
    let json = serde_json::to_string(line).map_err(io::Error::other)?;
    writeln!(w, "{json}")
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
/// required-ness is then enforced in one place (`qe_server::runs::manager`), which returns a uniform
/// `400` with a clear message for any missing/invalid param.
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
/// (`qe_server::runs::manager`) as a uniform `400`. The budget knobs are optional — `qe train` supplies
/// its own defaults when a flag is omitted. The **instrument/universe** is not a flag: `qe train`
/// resolves it from the config file (`--config`), so it is deliberately absent here.
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

    // ---- QE-458 whitelisted, gate-monotone steer knobs (design §6.1) ------------------------------
    /// **Indicator subset** — which catalogue-indicator ids are in play (`None` ⇒ the full catalogue).
    /// Restricting the subset is a *smaller* hypothesis space (strictly safer); the count feeds the
    /// distinct-trial basis `N` (QE-439, design §6.1a) so a larger subset only *raises* the deflation bar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indicator_subset: Option<Vec<String>>,
    /// **Evolved-pool source** — the id of an *already-sealed* evolved formula pool (QE-451) whose formulas
    /// may be referenced as indicators. Consuming it cannot un-seal or re-deflate it (read-only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evolved_pool: Option<String>,
    /// **Evolved-formula subset** — which `formula_hash`es of `evolved_pool` are in play (`None` ⇒ all sealed
    /// formulas). Their count adds to the available-feature-space size fed to `N` (design §6.1a).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evolved_formulas: Option<Vec<String>>,
    /// **WFO windows** — number of walk-forward windows the steered run scores over. More/longer windows
    /// raise `T_eff` and make CV *harder* to pass; a count below the regime-coverage floor is a `400`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub windows: Option<usize>,
    /// **CV folds** — number of cross-validation folds. A count below the regime-coverage floor is a `400`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folds: Option<usize>,

    // ---- QE-458 blocklist probes (design §6.2) — NOT steerable ------------------------------------
    // Everything the G1 gate's own decision rides on is off the whitelist and carries a compiled floor.
    // These fields exist ONLY so `validate_train` can reject (`400`) a client that tries to set one BELOW
    // its floor; setting one is never how a gate threshold changes (`G1Criteria`/`DEFLATION_BASIS_VERSION`
    // stay server-side, non-editable). All optional/lenient — an omitted probe is the normal case.
    /// Cost-stress friction multiplier (blocklist; floor `1×`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_stress_multiplier: Option<f64>,
    /// Max-turnover cap fraction (blocklist; floor `0.25`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turnover_frac: Option<f64>,
    /// Capacity floor, USD (blocklist; floor `250_000`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity_floor_usd: Option<i64>,
    /// DSR cutoff (blocklist; floor `0.95`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dsr_cutoff: Option<f64>,
    /// Uncensored-PBO cutoff (blocklist; floor `0.5`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pbo_cutoff: Option<f64>,
    /// IC / FDR discovery threshold (blocklist; floor `0.10`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ic_fdr_threshold: Option<f64>,
    /// Purge bars between train and holdout (blocklist; floored).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purge: Option<usize>,
}

// ---- QE-452 evolve run-spec (the GP indicator-evolution campaign) --------------------------------

/// Maximum `Expr`-tree depth an evolve campaign may declare (design §3/§13.4 caps). The grammar
/// (`ExprTree::repair`, QE-436) already enforces this on the engine side; a declared cap above it is a
/// leakage-inviting request and is rejected at create time.
pub const EVOLVE_MAX_DEPTH: usize = 4;
/// Maximum `Expr`-tree node count an evolve campaign may declare (design §3/§13.4 caps).
pub const EVOLVE_MAX_NODES: usize = 16;
/// Maximum indicator lookback (bars) an evolve campaign may declare (design §3/§13.4 caps).
pub const EVOLVE_MAX_LOOKBACK: usize = 200;
/// Maximum frozen-pool size `K` an evolve campaign may seal (design §3/§9; mirrors
/// `qe_wfo::gp::freeze::MAX_POOL_SIZE`).
pub const EVOLVE_MAX_POOL: usize = 16;
/// The fixed window-length lattice an evolve campaign's declared windows must lie on (design §13.4
/// guardrail chips).
pub const EVOLVE_WINDOW_LATTICE: [usize; 5] = [5, 10, 20, 50, 100];

/// The campaign mode of an evolve run (design §13.6). `sandbox` = RESEARCH (cannot reach a production
/// vintage — a physically separate artifacts root); `production` is only *launchable* once the compiled
/// prerequisite gate is satisfied (QE-454, not Phase A). Default `Sandbox` (fail-safe). An unknown mode
/// string is a serde reject → a clear `400`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvolveMode {
    /// Research mode — the pool is written to a separate research root and can never reach production.
    #[default]
    Sandbox,
    /// Production mode — only launchable when the prerequisite const gate is satisfied (QE-454).
    Production,
}

impl EvolveMode {
    /// The canonical wire string (`"sandbox"` / `"production"`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            EvolveMode::Sandbox => "sandbox",
            EvolveMode::Production => "production",
        }
    }
}

/// Evolve-campaign parameters (QE-452) — the `params` object of a `type:"evolve"` create-run request,
/// persisted verbatim in `meta.params` and mapped onto the `qe evolve` flags.
///
/// **`seed` is REQUIRED** (diverges from [`TrainParams`]' optional seed, design §13.10): a missing
/// `seed` is a serde reject (an evolve approval must stay byte-reproducible off the recorded seed). Every
/// **other** field is `#[serde(default)]` so the body otherwise parses leniently; the window
/// (`start`/`end`/`resolution`) required-ness and the caps (`depth≤4`, `nodes≤16`, `lookback≤200`,
/// `windows ∈ lattice`, `K≤16`) are enforced in one place (`qe_server::runs::manager::validate_evolve`)
/// as a uniform `400`, never a serde `422`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EvolveParams {
    /// Master illumination seed (**required**; `--seed`). Every per-offspring RNG stream derives from it
    /// (`task_rng`, QE-451), so a fixed seed + fixed snapshot reproduces the pool byte-identically.
    pub seed: u64,
    /// Campaign mode — `sandbox` (default) or `production`.
    #[serde(default)]
    pub mode: EvolveMode,
    /// Inclusive window start `YYYY-MM-DD` (required; `--start`).
    #[serde(default)]
    pub start: String,
    /// Exclusive window end `YYYY-MM-DD` (required; `--end`).
    #[serde(default)]
    pub end: String,
    /// Bar resolution (required; `--resolution`).
    #[serde(default)]
    pub resolution: String,
    /// Illumination generations (`--generations`); omitted ⇒ the CLI default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generations: Option<usize>,
    /// Offspring evaluated per generation (`--offspring`); omitted ⇒ the CLI default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offspring: Option<usize>,
    /// Quantiser state count for the trivial decision head (`--states`); omitted ⇒ the CLI default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub states: Option<u16>,
    /// Declared max tree depth (`--depth`); capped at [`EVOLVE_MAX_DEPTH`]. Omitted ⇒ the engine cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<usize>,
    /// Declared max tree node count (`--nodes`); capped at [`EVOLVE_MAX_NODES`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nodes: Option<usize>,
    /// Declared max indicator lookback in bars (`--lookback`); capped at [`EVOLVE_MAX_LOOKBACK`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lookback: Option<usize>,
    /// Declared window-length lattice (`--windows`); each entry must be in [`EVOLVE_WINDOW_LATTICE`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub windows: Option<Vec<usize>>,
    /// Frozen-pool size `K` (`--k`); capped at [`EVOLVE_MAX_POOL`]. Omitted ⇒ the CLI default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub k: Option<usize>,
    /// Optional config-file path override (`--config`); omitted ⇒ the CLI default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<String>,
    /// Optional operating profile override (`--profile`); omitted ⇒ the CLI default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
}

// ---- QE-460 composite flow run-spec (server-owned train→backtest over a frozen holdout) ----------

/// Composite-flow parameters (QE-460) — the `params` object of a `type:"flow"` create-run request
/// (design §5.1). A flow configures a steer-whitelisted `train` + its frozen OOS holdout **once**; the
/// server then sequences `train`→`backtest` in one supervised, atomic run with a deterministic
/// content-hash vintage handoff.
///
/// **`seed` is REQUIRED** (no serde default — mirrors [`EvolveParams::seed`], design §5.1: a flow verdict
/// must stay byte-reproducible off the recorded seed). The window (`start`/`end`/`resolution`) is required
/// too (enforced as a uniform `400` in `validate_flow`). **Every other field is `#[serde(default)]`** so the
/// body parses leniently.
///
/// The flow embeds the QE-458 steer-whitelisted train block (`generations`/`population`/`indicator_subset`/
/// `windows`/`folds`) + the blocklist probes + `holdout`/`embargo`/`purge` — it is a superset that yields a
/// [`TrainParams`] via [`FlowParams::to_train_params`], so `validate_flow` can **reuse** `validate_train`
/// verbatim (no duplicated/divergent whitelist). The instrument universe is config-derived like `train`; the
/// backtest window is **not** operator-chosen — it is the server-frozen holdout the train phase carves and
/// records (design §4 (a)/(b)).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FlowParams {
    /// Master flow seed (**required**; drives the train search seed + the deterministic backtest). A fixed
    /// seed + pinned snapshot reproduces the flow vintage byte-identically (QE-006/QE-129).
    pub seed: u64,
    /// Inclusive flow-window start `YYYY-MM-DD` (required; the train window's start).
    #[serde(default)]
    pub start: String,
    /// Exclusive flow-window end `YYYY-MM-DD` (required; the pinned snapshot's right edge — the holdout is
    /// carved server-side from it, design §4 (b)).
    #[serde(default)]
    pub end: String,
    /// Bar resolution (required; `1h`, …).
    #[serde(default)]
    pub resolution: String,
    /// MAP-Elites search generations (`--generations`); omitted ⇒ the CLI default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generations: Option<usize>,
    /// Variation steps per direction per generation (`--population`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub population: Option<usize>,
    /// Final bars reserved as the frozen G1 holdout (`--holdout`; floored, design §4). Carved once here and
    /// handed identically to both sub-runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holdout: Option<usize>,
    /// Embargo bars purged between the train window and the holdout (`--embargo`; floored, design §4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embargo: Option<usize>,
    /// Optional config-file path override (`--config`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<String>,
    /// Optional operating profile override (`--profile`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,

    // ---- QE-458 whitelisted, gate-monotone steer knobs (design §6.1) ------------------------------
    /// Indicator subset — catalogue-indicator ids in play (`None` ⇒ the full catalogue).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indicator_subset: Option<Vec<String>>,
    /// Evolved-pool source id (rejected by `validate_train`/`validate_flow` — not yet applied on the live
    /// search; carried so a request naming it errors rather than silently running un-steered).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evolved_pool: Option<String>,
    /// Evolved-formula subset (rejected, like `evolved_pool`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evolved_formulas: Option<Vec<String>>,
    /// WFO windows the steered run scores over (`--windows`; floored ≥ `MIN_WFO_WINDOWS`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub windows: Option<usize>,
    /// CV folds the steered run scores over (`--folds`; floored ≥ `MIN_WFO_FOLDS`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folds: Option<usize>,

    // ---- QE-458 blocklist probes (design §6.2) — NOT steerable ------------------------------------
    /// Cost-stress friction multiplier (blocklist; `validate_flow` rejects any set value).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_stress_multiplier: Option<f64>,
    /// Max-turnover cap fraction (blocklist).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turnover_frac: Option<f64>,
    /// Capacity floor, USD (blocklist).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity_floor_usd: Option<i64>,
    /// DSR cutoff (blocklist).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dsr_cutoff: Option<f64>,
    /// Uncensored-PBO cutoff (blocklist).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pbo_cutoff: Option<f64>,
    /// IC / FDR discovery threshold (blocklist).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ic_fdr_threshold: Option<f64>,
    /// Purge bars between train and holdout (blocklist; floored).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purge: Option<usize>,
}

impl FlowParams {
    /// Project the flow's steer-whitelisted train block onto a [`TrainParams`] (design §5.1) — the single
    /// source `validate_flow` validates and the flow supervisor spawns the `train` sub-job from, so the
    /// QE-458 whitelist/blocklist/floor enforcement is **reused, never duplicated**. The flow `seed` becomes
    /// the (required) train search seed.
    #[must_use]
    pub fn to_train_params(&self) -> TrainParams {
        TrainParams {
            start: self.start.clone(),
            end: self.end.clone(),
            resolution: self.resolution.clone(),
            seed: Some(self.seed),
            generations: self.generations,
            population: self.population,
            holdout: self.holdout,
            embargo: self.embargo,
            config: self.config.clone(),
            profile: self.profile.clone(),
            indicator_subset: self.indicator_subset.clone(),
            evolved_pool: self.evolved_pool.clone(),
            evolved_formulas: self.evolved_formulas.clone(),
            windows: self.windows,
            folds: self.folds,
            cost_stress_multiplier: self.cost_stress_multiplier,
            max_turnover_frac: self.max_turnover_frac,
            capacity_floor_usd: self.capacity_floor_usd,
            dsr_cutoff: self.dsr_cutoff,
            pbo_cutoff: self.pbo_cutoff,
            ic_fdr_threshold: self.ic_fdr_threshold,
            purge: self.purge,
        }
    }
}

/// Ingest parameters — the `params` object of a `type:"ingest"` create-run request (QE-464), persisted
/// verbatim in `meta.json` and mapped onto the `qe-cli ingest` flags.
///
/// Like the other run DTOs, every field is `#[serde(default)]` so the body parses **leniently**; the
/// required-ness (`start`/`end`/`resolution`, and either a non-empty `instruments` list **or**
/// `fetch_all`) is enforced in one place (`qe_server::runs::manager::validate_ingest`) as a uniform
/// `400`. `fetch_all` resolves the instrument set through the point-in-time universe machinery
/// (survivorship kill, QE-448); `synthetic` selects the deterministic offline generator (tagged
/// `synthetic`) instead of the real decoder (tagged `real`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct IngestParams {
    /// Explicit instrument symbols to ingest (`--instrument`, repeated). Empty ⇒ requires `fetch_all`.
    #[serde(default)]
    pub instruments: Vec<String>,
    /// Fetch **all** available instruments, resolved via the point-in-time universe (`--fetch-all`).
    #[serde(default)]
    pub fetch_all: bool,
    /// Inclusive window start `YYYY-MM-DD` (required; `--start`).
    #[serde(default)]
    pub start: String,
    /// Exclusive window end `YYYY-MM-DD` (required; `--end`).
    #[serde(default)]
    pub end: String,
    /// Bar resolution (required; `--resolution`).
    #[serde(default)]
    pub resolution: String,
    /// Generate a deterministic **offline synthetic** store instead of a real ingest (`--synthetic`);
    /// every bar is tagged `synthetic` so no store reads as real.
    #[serde(default)]
    pub synthetic: bool,
}

/// One occupied MAP-Elites niche of an evolve run's archive — the heatmap cell the QE-453 CampaignMonitor's
/// `ArchiveHeatmap` renders (design §13.4). The three descriptor axes are the pure-structural
/// family/timescale/complexity bands (§4.5); `best_fitness` is the cell champion's fitness (`None` when
/// non-finite, so a `-inf`/`NaN` never breaks JSON). Not a hashed artefact — plain `f64` is fine.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ArchiveCell {
    /// Dominant-family band label (one of the five `IndicatorFamily` variants).
    pub family: String,
    /// Structural-lookback timescale band label.
    pub timescale: String,
    /// Node-count complexity band label (`trivial`/`simple`/`complex`).
    pub complexity: String,
    /// The cell champion's node count.
    pub node_count: usize,
    /// The cell champion's fitness (`None` when non-finite).
    #[serde(default)]
    pub best_fitness: Option<f64>,
}

/// The GP-aware trial-count basis the QE-453 `TrialCountBar` renders (design §13.4/§13.5): the
/// distinct-canonical `N` against the analytic `cells·gens·windows` floor and the finite `E[maxSharpe]`
/// deflation bar (`N == floor` is the "QE-439 blind floor" tell the SPA highlights). Diagnostic only — the
/// authoritative deflation lives in the sealed pool's `DeflationSummary`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ArchiveTrialBasis {
    /// Distinct-canonical formulas evaluated (incl. rejects) — the QE-439 basis.
    pub distinct_evaluations: u64,
    /// The trial basis `N` the DSR deflated against (`max(distinct, analytic floor)`).
    pub n_trials: u64,
    /// The analytic `cells·gens·windows` floor (so an `N == floor` blind-floor is visible).
    pub analytic_floor: u64,
    /// The finite best-of-`N` noise Sharpe bar (`None` when non-finite).
    #[serde(default)]
    pub expected_max_sharpe: Option<f64>,
    /// Occupied niches in the archive.
    pub occupied_cells: usize,
    /// Total niches in the grid (`5×3×3 = 45`).
    pub total_cells: usize,
}

/// The `archive.json` sidecar an evolve run writes into its run dir (QE-452 Phase B) — the MAP-Elites
/// archive snapshot the QE-453 CampaignMonitor consumes, served by `GET /api/runs/{id}/archive`. A shared
/// DTO in this leaf so the CLI producer, the server route, and the SPA read one shape. Deterministic for a
/// fixed seed (cells in sorted-cell order); not a hashed artefact.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EvolveArchive {
    /// The sealed pool id this run produced (deep-link join key).
    pub pool_id: String,
    /// The campaign mode (`sandbox` / `production`).
    pub mode: String,
    /// Illumination generations run.
    pub generations: usize,
    /// Offspring per generation.
    pub offspring: usize,
    /// The occupied heatmap cells (sorted-cell order).
    #[serde(default)]
    pub cells: Vec<ArchiveCell>,
    /// The trial-count deflation basis bars.
    pub trial_basis: ArchiveTrialBasis,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_line_serialises_with_t_tag() {
        let mut buf = Vec::new();
        emit_progress(&mut buf, 50, "features", "assembling").unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert_eq!(
            s.trim_end(),
            r#"{"t":"progress","pct":50,"stage":"features","msg":"assembling"}"#
        );
    }

    #[test]
    fn done_stamps_protocol_version_and_error_line() {
        let mut buf = Vec::new();
        emit_done(&mut buf, "result.json").unwrap();
        emit_error(&mut buf, "boom").unwrap();
        let s = String::from_utf8(buf).unwrap();
        let mut lines = s.lines();
        assert_eq!(
            lines.next().unwrap(),
            r#"{"t":"done","result":"result.json","protocol_version":3}"#
        );
        assert_eq!(lines.next().unwrap(), r#"{"t":"error","msg":"boom"}"#);
    }

    #[test]
    fn backtest_params_defaults_match_cli() {
        let p = BacktestParams::default();
        assert_eq!(p.taker_fee_bps, 2.0);
        assert_eq!(p.slippage_model, "square-root-impact");
    }

    #[test]
    fn protocol_version_is_three() {
        assert_eq!(
            PROTOCOL_VERSION, 3,
            "QE-460 bumped the run protocol 2 → 3 for the new `flow` run-kind"
        );
    }

    #[test]
    fn evolve_done_carries_pool_and_never_vintage() {
        let mut buf = Vec::new();
        emit_evolve_done(&mut buf, "result.json", "pool-abc123").unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert_eq!(
            s.trim_end(),
            r#"{"t":"done","result":"result.json","protocol_version":3,"pool":"pool-abc123"}"#
        );
        // Round-trips back to a Done carrying the pool, no vintage.
        match serde_json::from_str::<ProgressLine>(s.trim_end()).unwrap() {
            ProgressLine::Done { pool, vintage, .. } => {
                assert_eq!(pool.as_deref(), Some("pool-abc123"));
                assert_eq!(vintage, None);
            }
            other => panic!("expected Done, got {other:?}"),
        }
    }

    #[test]
    fn evolve_params_requires_seed_but_defaults_the_rest() {
        // A missing seed is a serde reject (seed is REQUIRED).
        let err = serde_json::from_str::<EvolveParams>(r#"{"start":"a"}"#).unwrap_err();
        assert!(err.to_string().contains("seed"), "missing seed: {err}");
        // With just a seed, every other field takes its default.
        let p: EvolveParams = serde_json::from_str(r#"{"seed":7}"#).unwrap();
        assert_eq!(p.seed, 7);
        assert_eq!(p.mode, EvolveMode::Sandbox);
        assert_eq!(p.k, None);
        assert!(p.windows.is_none());
        // Unknown mode string is a serde reject → a clear 400 upstream.
        let bad = serde_json::from_str::<EvolveParams>(r#"{"seed":1,"mode":"prod"}"#);
        assert!(bad.is_err(), "unknown mode must reject");
    }

    #[test]
    fn flow_params_requires_seed_but_defaults_the_rest() {
        // A missing seed is a serde reject (seed is REQUIRED, mirroring EvolveParams).
        let err = serde_json::from_str::<FlowParams>(r#"{"start":"2021-01-01"}"#).unwrap_err();
        assert!(err.to_string().contains("seed"), "missing seed: {err}");
        // With just a seed, every other field takes its default.
        let p: FlowParams = serde_json::from_str(r#"{"seed":9}"#).unwrap();
        assert_eq!(p.seed, 9);
        assert!(p.start.is_empty());
        assert_eq!(p.holdout, None);
    }

    #[test]
    fn flow_to_train_params_carries_the_seed_window_and_steer_block() {
        let p: FlowParams = serde_json::from_str(
            r#"{"seed":9,"start":"2021-01-01","end":"2021-06-01","resolution":"1h",
                "holdout":300,"embargo":24,"windows":6,"folds":3,
                "indicator_subset":["rsi_14"]}"#,
        )
        .unwrap();
        let t = p.to_train_params();
        // The flow seed becomes the (required) train search seed, and the steer block is carried verbatim so
        // `validate_train` can be reused with no divergence.
        assert_eq!(t.seed, Some(9));
        assert_eq!(t.start, "2021-01-01");
        assert_eq!(t.holdout, Some(300));
        assert_eq!(t.embargo, Some(24));
        assert_eq!(t.windows, Some(6));
        assert_eq!(t.folds, Some(3));
        assert_eq!(
            t.indicator_subset.as_deref(),
            Some(&["rsi_14".to_owned()][..])
        );
    }
}
