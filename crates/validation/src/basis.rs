//! QE-454 Phase B — the compiled **deflation-basis prerequisite gate** (design §13.6 barrier 1).
//!
//! [`DEFLATION_BASIS_VERSION`] is a **5-bit set**, one bit per landed prerequisite ticket (design §13.13
//! open-q1 "lean: bitset" — a partial landing is representable and the eligibility report can name exactly
//! which prereq is missing). It is a **source const**, bumped **only** in the prerequisite landing commit
//! (provable from `Lineage.code_commit`); it feeds **no hashed artefact field**, so it can gate the seal
//! route without touching any golden.
//!
//! The production evolve path is unlocked purely by this const reaching [`REQUIRED_DEFLATION_BASIS`]:
//! `validate_evolve` refuses to *launch* a `production` campaign while [`deflation_basis_satisfied`] is
//! `false` (a tampered client cannot even start one), and the server-authoritative `seal_allowed` refuses
//! to *seal* one — the const is one of its three inputs.
//!
//! The five prerequisites (QE-450 §13.6): **QE-430** ensemble-correlation deflation, **QE-432** slow
//! reference oracle, **QE-434** IC screening, **QE-436** parsimony/MDL, **QE-439** DSR trial basis. All
//! five are **merged on `main`** ahead of this phase, so [`DEFLATION_BASIS_VERSION`] is set to the
//! fully-satisfied value ([`REQUIRED_DEFLATION_BASIS`]).

/// Bit: QE-430 ensemble-correlation deflation landed.
pub const BASIS_QE430_ENSEMBLE_CORR: u32 = 1 << 0;
/// Bit: QE-432 slow reference oracle landed.
pub const BASIS_QE432_REFERENCE_ORACLE: u32 = 1 << 1;
/// Bit: QE-434 IC screening landed.
pub const BASIS_QE434_IC_SCREEN: u32 = 1 << 2;
/// Bit: QE-436 parsimony / MDL landed.
pub const BASIS_QE436_MDL: u32 = 1 << 3;
/// Bit: QE-439 DSR trial basis landed.
pub const BASIS_QE439_TRIAL_BASIS: u32 = 1 << 4;

/// The full set of prerequisite bits a production evolve campaign requires (design §13.6). Production is
/// unlockable **only** when [`DEFLATION_BASIS_VERSION`] carries every one of these.
pub const REQUIRED_DEFLATION_BASIS: u32 = BASIS_QE430_ENSEMBLE_CORR
    | BASIS_QE432_REFERENCE_ORACLE
    | BASIS_QE434_IC_SCREEN
    | BASIS_QE436_MDL
    | BASIS_QE439_TRIAL_BASIS;

/// The **compiled** deflation-basis version — the prerequisite bits this build sanctions (design §13.6
/// barrier 1). Bumped **only** in the prerequisite landing commit. All five prereqs (QE-430/432/434/436/439)
/// are merged on `main` ahead of QE-454 Phase B, so this is the fully-satisfied [`REQUIRED_DEFLATION_BASIS`].
///
/// **Not a hashed field.** This gates the seal route + the production-launch check only; it is never folded
/// into a vintage/pool content hash, so bumping it moves no golden.
pub const DEFLATION_BASIS_VERSION: u32 = REQUIRED_DEFLATION_BASIS;

/// Whether the compiled basis satisfies every production prerequisite (`const & REQUIRED == REQUIRED`).
/// `validate_evolve` and `seal_allowed` both gate on this; when `false`, no production campaign can launch
/// or seal.
#[must_use]
pub fn deflation_basis_satisfied() -> bool {
    basis_satisfied(DEFLATION_BASIS_VERSION)
}

/// Whether `version` carries every [`REQUIRED_DEFLATION_BASIS`] bit (pure, so it is testable at partial
/// landings without mutating the compiled const).
#[must_use]
pub fn basis_satisfied(version: u32) -> bool {
    version & REQUIRED_DEFLATION_BASIS == REQUIRED_DEFLATION_BASIS
}

/// The names of the prerequisite bits **missing** from `version` (empty iff satisfied) — the eligibility
/// report the SPA / a `409` blocker can name exactly (design §13.13 open-q1).
#[must_use]
pub fn missing_basis_prereqs(version: u32) -> Vec<&'static str> {
    const PREREQS: [(u32, &str); 5] = [
        (
            BASIS_QE430_ENSEMBLE_CORR,
            "QE-430 ensemble-correlation deflation",
        ),
        (BASIS_QE432_REFERENCE_ORACLE, "QE-432 slow reference oracle"),
        (BASIS_QE434_IC_SCREEN, "QE-434 IC screening"),
        (BASIS_QE436_MDL, "QE-436 parsimony/MDL"),
        (BASIS_QE439_TRIAL_BASIS, "QE-439 DSR trial basis"),
    ];
    PREREQS
        .iter()
        .filter(|(bit, _)| version & bit == 0)
        .map(|(_, name)| *name)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiled_basis_satisfies_every_prerequisite() {
        // The prereqs (QE-430/432/434/436/439) are merged on main ahead of this phase, so the compiled
        // const clears the production gate.
        assert!(deflation_basis_satisfied());
        assert_eq!(DEFLATION_BASIS_VERSION, REQUIRED_DEFLATION_BASIS);
        assert_eq!(DEFLATION_BASIS_VERSION, 0b11111);
        assert!(missing_basis_prereqs(DEFLATION_BASIS_VERSION).is_empty());
    }

    #[test]
    fn a_partial_landing_is_not_satisfied_and_names_the_gap() {
        // Drop the QE-439 trial-basis bit: production must NOT unlock, and the report must name it.
        let partial = REQUIRED_DEFLATION_BASIS & !BASIS_QE439_TRIAL_BASIS;
        assert!(!basis_satisfied(partial));
        let missing = missing_basis_prereqs(partial);
        assert_eq!(missing, vec!["QE-439 DSR trial basis"]);

        // The blind zero-basis (nothing landed) names all five.
        assert!(!basis_satisfied(0));
        assert_eq!(missing_basis_prereqs(0).len(), 5);
    }
}
