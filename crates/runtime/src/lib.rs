//! qe-runtime — thin **facade** over the split runtime crates (QE-426).
//!
//! QE-426 split the former `qe-runtime` god-crate along the spec's process seams into
//! [`qe_runtime_core`] (the shared planner⑤ ↔ edge⑥ contract), [`qe_hedger`] (Bootstrap ③ + Live ④ + Hedge
//! Planning ⑤), and [`qe_edge`] (the Edge gateway ⑥ — venue adapter / position keeper / kill gate / order
//! submission). The gRPC seam (QE-218) between the planner and the adapter is now a **crate** boundary, so
//! the order-submitting code (`qe-edge`) compiles independently, is independently panic-free-lint-scoped
//! (QE-268), and is independently deployable.
//!
//! This crate re-exports the full prior public API — types **and** module paths (`qe_runtime::boot_state::…`,
//! `qe_runtime::transport::…`, …) — so every downstream `use qe_runtime::X` keeps compiling unchanged. New
//! code should prefer depending on the specific split crate.

// The full runtime surface, re-exported from the split crates. Glob re-exports carry both the flat type
// re-exports and the `pub mod` module paths (so `qe_runtime::<module>::<Type>` still resolves). The three
// contract types (`TargetPosition`/`CapitalView`/`PositionKeeper`) reach the facade via `qe_hedger`, which
// re-exports them from `qe-runtime-core` — so no separate `qe_runtime_core` glob is needed here.
pub use qe_edge::*;
pub use qe_hedger::*;

// The QE-009 kill contract types, re-exported as before (they were `pub use qe_risk::{KillHandle, KillSwitch}`
// in the pre-split crate).
pub use qe_risk::{KillHandle, KillSwitch};

/// Returns this crate's package name. Placeholder kept from the pre-split scaffold (QE-001).
#[must_use]
pub fn crate_name() -> &'static str {
    "qe-runtime"
}

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_set() {
        assert_eq!(super::crate_name(), "qe-runtime");
    }
}
