//! qe-hedger — the planner side of the runtime: Bootstrap ③ + Live pipeline ④ + Hedge Planning ⑤ (QE-426).
//!
//! Split out of `qe-runtime` so the gRPC seam (QE-218) to the Edge gateway ⑥ (`qe-edge`) is a **crate**
//! boundary. The planner replays the lookback window, runs the sealed vintage through the evaluator, breaks
//! and nets per strategy, and emits an **absolute** [`TargetPosition`](qe_runtime_core::TargetPosition) over
//! the shared [`qe_runtime_core`] contract — it never links the order-submission adapter (`qe-edge`).
//!
//! - Bootstrap ③: [`boot_state`], [`bootstrap`], [`cutover`] — cold-start replay + in-place cutover.
//! - Live ④: [`live_kline`], [`live_mark`], [`factor_join`], [`evaluator`], [`live_breakers`],
//!   [`live_netter`] — the continuous replay→live evaluation stack.
//! - Hedge ⑤: [`hedger`] (the [`HedgePlanner`](hedger::HedgePlanner)), [`pretrade`] (the QE-215 governor),
//!   [`vintage_rollover`] (the QE-219 lifecycle record).

pub mod boot_state;
pub mod bootstrap;
pub mod cutover;
pub mod evaluator;
pub mod factor_join;
pub mod hedger;
pub mod live_breakers;
pub mod live_kline;
pub mod live_mark;
pub mod live_netter;
pub mod pretrade;
pub mod vintage_rollover;

// QE-421 recoverability taxonomy: the hedger-side `Classified` impls (bootstrap/cutover/reconstruction). No
// public items — the module exists to compile the trait impls in the crate that owns the error types.
mod classify;

pub use boot_state::{
    BootStateError, CommittedPeak, DormancyLatch, ReconstructedState, StrategyState,
};
pub use bootstrap::{
    paginate_klines, paginate_series, BootstrapError, BootstrapPipeline, HistoricalSource,
    HistoricalWindow, Reconstructed,
};
pub use cutover::{Cutover, CutoverError, CutoverStep};
pub use evaluator::{ChromosomeDecision, EvalOutput, EvaluatorSession, SessionMode};
pub use factor_join::LiveFactorJoin;
pub use hedger::HedgePlanner;
pub use live_breakers::BreakerLayer;
pub use live_kline::LiveKlineSource;
pub use live_mark::{
    MarkEmaConfig, MarkEmaLoop, MarkTick, MarkTickObserver, DEFAULT_HALF_LIFE_SECS,
    DEFAULT_STALENESS_BOUND_SECS, DEFAULT_TICK_SECS,
};
pub use live_netter::{NetLeg, NetTarget, PositionNetter};
pub use pretrade::{PreTradeDecision, PreTradeGovernor, PreTradeVerdict};
pub use vintage_rollover::{ActiveVintage, RolloverRecord};

// The QE-214 planner contract now lives in `qe-runtime-core`; re-exported here so the planner's public API
// (and the `qe-runtime` facade) surface the same names as before the split.
pub use qe_runtime_core::{CapitalView, PositionKeeper, TargetPosition};
