//! Strategy genome representation (QE-110) — the fixed structure the whole evolutionary search
//! mutates, recombines, and niches.
//!
//! **Representation (decision QE-110/D1).** A *fixed-structure parameter vector that encodes a
//! bounded rule set over quantised indicator states* — the hybrid that is the strict superset both
//! downstreams need: DE/MAP-Elites (QE-115/118/126) require fixed, position-wise loci, while the spec
//! frames strategies as logic over quantised states. The genome is a fixed-length vector of typed
//! genes whose genes encode a **k-of-n band rule set**, grouped into **per-direction entry banks**
//! (so the archive can be per-direction, QE-111) plus fixed exit/risk/holding genes.
//!
//! **Determinism.** [`Genome::decide`] is a pure function of `(genome, features, position)` — no RNG,
//! no clock, no hidden state — so it is identical batch vs streaming and reproducible (QE-006).
//!
//! **Operator surface (for QE-112/QE-119).** Operators "mutate freely, then [`Genome::repair`]":
//! mutation/DE may push genes out of domain, and `repair` deterministically clamps them back onto the
//! validity manifold (idempotent). The gene layout is fixed and documented so operators can enumerate
//! loci for uniform/typed mutation and per-locus crossover.

use std::collections::BTreeSet;

use crate::{FeatureSchema, FeatureVector};
use qe_domain::Direction;
use serde::{Deserialize, Serialize};

/// Representation version, stamped into every genome for lineage and decode-mismatch safety. A future
/// representation change bumps this so an old genome decodes loudly rather than being misread.
pub const REP_VERSION: u16 = 1;

/// Fixed number of clauses in each entry rule set. `enabled` flags let the *effective* clause count
/// evolve without changing genome length.
pub const CLAUSES_PER_SET: usize = 4;

/// Target notional ceiling, in basis points of allowed capital (`risk.size_bps` upper bound).
pub const MAX_SIZE_BPS: u16 = 10_000;

/// One band condition over a single quantised indicator state: "feature `feature`'s state is in the
/// inclusive band `[lo, hi]`". Disabled clauses are ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Clause {
    /// Whether this clause participates (operators toggle this to evolve the effective rule count).
    pub enabled: bool,
    /// Index into the [`FeatureSchema`] — which indicator's quantised state this clause reads.
    pub feature: u16,
    /// Inclusive lower state bound.
    pub lo: u16,
    /// Inclusive upper state bound (`lo ≤ hi < num_states`).
    pub hi: u16,
}

impl Clause {
    /// Whether this clause is satisfied by `features`: enabled, the referenced feature is **warm**
    /// (a present state), and its state index is within `[lo, hi]`. An out-of-range index or a
    /// not-yet-warm slot is unsatisfied (conservative — never fires on missing data).
    #[must_use]
    pub fn satisfied(&self, features: &FeatureVector) -> bool {
        if !self.enabled {
            return false;
        }
        match features.states.get(self.feature as usize) {
            Some(Some(state)) => {
                let s = state.index();
                self.lo <= s && s <= self.hi
            }
            _ => false,
        }
    }
}

/// A `k`-of-`n` rule bank: fires when at least `min_satisfied` of its **active** (enabled) clauses are
/// satisfied. An all-disabled bank never fires.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleSet {
    /// The fixed clause array.
    pub clauses: [Clause; CLAUSES_PER_SET],
    /// `k` in "k-of-active"; at evaluation it is clamped into `1..=active_count`.
    pub min_satisfied: u8,
}

impl RuleSet {
    /// Number of enabled clauses.
    #[must_use]
    pub fn active_count(&self) -> usize {
        self.clauses.iter().filter(|c| c.enabled).count()
    }

    /// Whether the bank fires for `features`. Threshold = `min(min_satisfied, active_count)` so a bank
    /// with few enabled clauses still fires sensibly; an empty (no active clauses) bank never fires.
    #[must_use]
    pub fn fires(&self, features: &FeatureVector) -> bool {
        let active = self.active_count();
        if active == 0 {
            return false;
        }
        let satisfied = self
            .clauses
            .iter()
            .filter(|c| c.satisfied(features))
            .count();
        let threshold = (self.min_satisfied as usize).clamp(1, active);
        satisfied >= threshold
    }
}

/// Exit genes: a hard holding cap plus an optional opposite-signal exit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExitParams {
    /// Exit once a position has been held this many bars (`≥ 1`).
    pub max_holding_bars: u16,
    /// If set, exit when the opposite direction's entry bank fires.
    pub exit_on_opposite: bool,
}

/// Risk / sizing genes. Hard stops and breakers live in the runtime/risk layer (QE-116/QE-212), not
/// in the search genome — deliberately, so evolution cannot overfit stop placement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiskParams {
    /// Target notional as basis points of allowed capital (`1..=MAX_SIZE_BPS`). The backtester
    /// (QE-120) reads this on entry; it is intentionally *not* part of [`Decision`].
    pub size_bps: u16,
}

/// The current position the evaluator is asked about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PositionState {
    /// Held direction, or `None` when flat.
    pub dir: Option<Direction>,
    /// Bars the position has been held (0 on the entry bar).
    pub bars_held: u16,
}

impl PositionState {
    /// A flat position.
    #[must_use]
    pub fn flat() -> Self {
        PositionState {
            dir: None,
            bars_held: 0,
        }
    }

    /// A position held in `dir` for `bars_held` bars.
    #[must_use]
    pub fn held(dir: Direction, bars_held: u16) -> Self {
        PositionState {
            dir: Some(dir),
            bars_held,
        }
    }
}

/// The genome's per-bar output: a pure trading signal. Position size is *not* here — the backtester
/// reads [`RiskParams::size_bps`] on entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Do nothing this bar.
    Hold,
    /// Open a position in the given direction (only emitted when flat).
    Enter(Direction),
    /// Close the current position.
    Exit,
}

/// A strategy genome: per-direction entry banks plus exit/risk genes, over a fixed structure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Genome {
    /// Representation version (see [`REP_VERSION`]).
    pub version: u16,
    /// Entry bank whose firing permits a **long** when flat.
    pub long_entry: RuleSet,
    /// Entry bank whose firing permits a **short** when flat.
    pub short_entry: RuleSet,
    /// Exit genes.
    pub exit: ExitParams,
    /// Risk / sizing genes.
    pub risk: RiskParams,
}

impl Genome {
    /// Decide the action for `features` given the current `position`.
    ///
    /// **Flat:** the long bank firing alone ⇒ `Enter(Long)`, the short bank alone ⇒ `Enter(Short)`;
    /// both or neither ⇒ `Hold` (an ambiguous or absent signal never enters — no accidental net-long
    /// bias). **In position:** `Exit` once `bars_held ≥ max_holding_bars`, or (when `exit_on_opposite`)
    /// when the opposite bank fires; otherwise `Hold`.
    #[must_use]
    pub fn decide(&self, features: &FeatureVector, position: PositionState) -> Decision {
        match position.dir {
            None => match (
                self.long_entry.fires(features),
                self.short_entry.fires(features),
            ) {
                (true, false) => Decision::Enter(Direction::Long),
                (false, true) => Decision::Enter(Direction::Short),
                _ => Decision::Hold,
            },
            Some(dir) => {
                if position.bars_held >= self.exit.max_holding_bars {
                    return Decision::Exit;
                }
                if self.exit.exit_on_opposite {
                    let opposite = match dir {
                        Direction::Long => &self.short_entry,
                        Direction::Short => &self.long_entry,
                    };
                    if opposite.fires(features) {
                        return Decision::Exit;
                    }
                }
                Decision::Hold
            }
        }
    }

    /// Whether every gene is within its valid domain for `schema` (see QE-110/D5). On an empty schema
    /// the per-clause feature/state bounds are vacuous and skipped.
    #[must_use]
    pub fn is_valid(&self, schema: &FeatureSchema) -> bool {
        if !(1..=MAX_SIZE_BPS).contains(&self.risk.size_bps) {
            return false;
        }
        if self.exit.max_holding_bars == 0 {
            return false;
        }
        let len = schema.len();
        let num_states = schema.num_states();
        for set in [&self.long_entry, &self.short_entry] {
            let ms = set.min_satisfied as usize;
            if !(1..=CLAUSES_PER_SET).contains(&ms) {
                return false;
            }
            if len == 0 {
                continue;
            }
            for c in &set.clauses {
                if c.feature as usize >= len || c.hi >= num_states || c.lo > c.hi {
                    return false;
                }
            }
        }
        true
    }

    /// Deterministically clamp every gene back onto the validity manifold for `schema`. Idempotent
    /// (`repair` twice == once) and the contract operators rely on after free mutation / DE arithmetic.
    /// `version` is normalised to [`REP_VERSION`].
    pub fn repair(&mut self, schema: &FeatureSchema) {
        self.version = REP_VERSION;
        self.risk.size_bps = self.risk.size_bps.clamp(1, MAX_SIZE_BPS);
        self.exit.max_holding_bars = self.exit.max_holding_bars.max(1);

        let len = schema.len();
        let max_feature = (len.saturating_sub(1)) as u16;
        let max_state = schema.num_states().saturating_sub(1);
        for set in [&mut self.long_entry, &mut self.short_entry] {
            set.min_satisfied = set.min_satisfied.clamp(1, CLAUSES_PER_SET as u8);
            if len == 0 {
                continue;
            }
            for c in &mut set.clauses {
                if c.feature > max_feature {
                    c.feature = max_feature;
                }
                if c.hi > max_state {
                    c.hi = max_state;
                }
                if c.lo > c.hi {
                    c.lo = c.hi;
                }
            }
        }
    }

    /// The set of feature indices referenced by **enabled** clauses across both banks — the
    /// genotype-derived input QE-111 reads for structural behaviour descriptors (indicator family /
    /// timescale), stable across re-evaluation windows.
    #[must_use]
    pub fn referenced_features(&self) -> BTreeSet<u16> {
        let mut out = BTreeSet::new();
        for set in [&self.long_entry, &self.short_entry] {
            for c in set.clauses.iter().filter(|c| c.enabled) {
                out.insert(c.feature);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CatalogueConfig, QState};

    // --- helpers -----------------------------------------------------------------------------

    /// Build a feature vector from explicit state indices (`None` = not warm).
    fn fv(states: &[Option<u16>]) -> FeatureVector {
        FeatureVector {
            time_ms: 0,
            states: states.iter().map(|o| o.map(QState::from_index)).collect(),
        }
    }

    fn clause(enabled: bool, feature: u16, lo: u16, hi: u16) -> Clause {
        Clause {
            enabled,
            feature,
            lo,
            hi,
        }
    }

    fn disabled() -> Clause {
        clause(false, 0, 0, 0)
    }

    /// The reference fixture genome documented in `docs/architecture/qe-110-...-design.md` (D3 trace).
    /// Over 3 features (states 0..=4): long when f0 AND f1 are high `[3,4]`; short when f0 AND f1 are
    /// low `[0,1]`; exit after 3 bars or on an opposite signal.
    fn fixture_genome() -> Genome {
        Genome {
            version: REP_VERSION,
            long_entry: RuleSet {
                clauses: [
                    clause(true, 0, 3, 4),
                    clause(true, 1, 3, 4),
                    disabled(),
                    disabled(),
                ],
                min_satisfied: 2,
            },
            short_entry: RuleSet {
                clauses: [
                    clause(true, 0, 0, 1),
                    clause(true, 1, 0, 1),
                    disabled(),
                    disabled(),
                ],
                min_satisfied: 2,
            },
            exit: ExitParams {
                max_holding_bars: 3,
                exit_on_opposite: true,
            },
            risk: RiskParams { size_bps: 5_000 },
        }
    }

    // --- AC: fixture evaluates to the documented decisions ------------------------------------

    #[test]
    fn fixture_matches_hand_traced_decisions() {
        let g = fixture_genome();
        let flat = PositionState::flat();

        // A — flat, both high → Enter(Long).
        assert_eq!(
            g.decide(&fv(&[Some(4), Some(4), Some(0)]), flat),
            Decision::Enter(Direction::Long)
        );
        // B — flat, only f0 high (k-of-n short of 2) → Hold.
        assert_eq!(
            g.decide(&fv(&[Some(4), Some(2), Some(0)]), flat),
            Decision::Hold
        );
        // C — flat, both low → Enter(Short).
        assert_eq!(
            g.decide(&fv(&[Some(0), Some(0), Some(0)]), flat),
            Decision::Enter(Direction::Short)
        );
        // E — long, held to the cap (3) → Exit (max holding).
        assert_eq!(
            g.decide(
                &fv(&[Some(4), Some(4), Some(0)]),
                PositionState::held(Direction::Long, 3)
            ),
            Decision::Exit
        );
        // F — long, opposite (short) bank fires → Exit (opposite signal).
        assert_eq!(
            g.decide(
                &fv(&[Some(0), Some(0), Some(0)]),
                PositionState::held(Direction::Long, 1)
            ),
            Decision::Exit
        );
        // G — long, neither cap nor opposite → Hold.
        assert_eq!(
            g.decide(
                &fv(&[Some(4), Some(2), Some(0)]),
                PositionState::held(Direction::Long, 1)
            ),
            Decision::Hold
        );
    }

    // --- k-of-n firing, enabled toggling, warmth ---------------------------------------------

    #[test]
    fn k_of_n_threshold_and_enabled_govern_firing() {
        // Two active clauses, k=1: either satisfied fires.
        let bank = RuleSet {
            clauses: [
                clause(true, 0, 3, 4),
                clause(true, 1, 3, 4),
                disabled(),
                disabled(),
            ],
            min_satisfied: 1,
        };
        assert!(bank.fires(&fv(&[Some(4), Some(0), None]))); // only c0
        assert!(bank.fires(&fv(&[Some(0), Some(4), None]))); // only c1
        assert!(!bank.fires(&fv(&[Some(0), Some(0), None]))); // neither

        // Same clauses, k=2: needs both.
        let strict = RuleSet {
            min_satisfied: 2,
            ..bank.clone()
        };
        assert!(!strict.fires(&fv(&[Some(4), Some(0), None])));
        assert!(strict.fires(&fv(&[Some(4), Some(4), None])));
    }

    #[test]
    fn all_disabled_bank_never_fires() {
        let bank = RuleSet {
            clauses: [disabled(), disabled(), disabled(), disabled()],
            min_satisfied: 1,
        };
        assert_eq!(bank.active_count(), 0);
        assert!(!bank.fires(&fv(&[Some(0), Some(0), Some(0)])));
    }

    #[test]
    fn not_warm_slot_is_unsatisfied() {
        let c = clause(true, 0, 0, 4);
        assert!(!c.satisfied(&fv(&[None]))); // present index, no state
        assert!(!c.satisfied(&fv(&[]))); // index out of range
        assert!(c.satisfied(&fv(&[Some(2)])));
    }

    #[test]
    fn both_banks_firing_is_ambiguous_hold() {
        // A genome where the same condition arms both directions → flat input fires both → Hold.
        let g = Genome {
            long_entry: RuleSet {
                clauses: [clause(true, 0, 2, 2), disabled(), disabled(), disabled()],
                min_satisfied: 1,
            },
            short_entry: RuleSet {
                clauses: [clause(true, 0, 2, 2), disabled(), disabled(), disabled()],
                min_satisfied: 1,
            },
            ..fixture_genome()
        };
        assert_eq!(
            g.decide(&fv(&[Some(2)]), PositionState::flat()),
            Decision::Hold
        );
    }

    // --- validity / repair -------------------------------------------------------------------

    fn schema() -> FeatureSchema {
        // The real catalogue schema: a known width and num_states (default config → 5 states).
        FeatureSchema::from_catalogue(&CatalogueConfig { states: 5 })
    }

    #[test]
    fn fixture_is_valid() {
        let s = schema();
        assert!(s.len() >= 3 && s.num_states() == 5);
        assert!(fixture_genome().is_valid(&s));
    }

    #[test]
    fn repair_clamps_out_of_domain_genes_and_is_idempotent() {
        let s = schema();
        let bad_feature = (s.len() + 100) as u16;
        let mut g = Genome {
            version: 999,
            long_entry: RuleSet {
                clauses: [
                    clause(true, bad_feature, 0, 0), // feature out of range
                    clause(true, 0, 9, 2),           // hi ≥ num_states AND lo > hi
                    disabled(),
                    disabled(),
                ],
                min_satisfied: 0, // below range
            },
            short_entry: RuleSet {
                clauses: [clause(true, 1, 0, 99), disabled(), disabled(), disabled()],
                min_satisfied: 9, // above range
            },
            exit: ExitParams {
                max_holding_bars: 0, // below range
                exit_on_opposite: false,
            },
            risk: RiskParams { size_bps: 0 }, // below range
        };
        assert!(!g.is_valid(&s));

        g.repair(&s);
        assert!(g.is_valid(&s));
        assert_eq!(g.version, REP_VERSION);
        assert_eq!(g.risk.size_bps, 1);
        assert_eq!(g.exit.max_holding_bars, 1);

        // Idempotent.
        let once = g.clone();
        g.repair(&s);
        assert_eq!(g, once);
    }

    #[test]
    fn genome_never_fires_when_no_features_are_present() {
        // Defensive: a feature vector with no slots (the degenerate-schema case) never fires either
        // bank, so the genome holds rather than panicking on missing data.
        let g = fixture_genome();
        assert_eq!(g.decide(&fv(&[]), PositionState::flat()), Decision::Hold);
        assert!(!g.long_entry.fires(&fv(&[])));
        assert!(!g.short_entry.fires(&fv(&[])));
    }

    // --- determinism + serde -----------------------------------------------------------------

    #[test]
    fn decide_is_pure_and_repeatable() {
        let g = fixture_genome();
        let f = fv(&[Some(4), Some(4), Some(0)]);
        let first = g.decide(&f, PositionState::flat());
        for _ in 0..5 {
            assert_eq!(g.decide(&f, PositionState::flat()), first);
        }
    }

    #[test]
    fn serde_json_round_trips() {
        let g = fixture_genome();
        let json = serde_json::to_string(&g).expect("serialise");
        assert!(json.contains("\"version\""));
        let back: Genome = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back, g);
        assert_eq!(back.version, REP_VERSION);
    }

    #[test]
    fn referenced_features_are_enabled_only() {
        let g = fixture_genome();
        // Enabled clauses reference features 0 and 1 in both banks.
        assert_eq!(g.referenced_features(), BTreeSet::from([0, 1]));
    }
}
