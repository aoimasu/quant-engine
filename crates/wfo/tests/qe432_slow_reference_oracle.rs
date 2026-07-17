//! QE-432 — Independent slow-reference oracle for the reconstruct roll-up & net-of-cost fitness.
//!
//! Dubno §5.5: "make a backtester that is slow but works, then verify the optimised version matches it
//! exactly." Every existing parity guarantee in the engine is same-code-vs-same-code (the determinism
//! harness re-runs one closure; batch/streaming parity drives the *same* reconstructor) — a shared logic
//! bug would reproduce byte-for-byte and corrupt every vintage undetected. This test adds the missing
//! **independent** oracle: a deliberately naive re-derivation of (a) the multi-resolution bar roll-up and
//! (b) the wfo cost-ledger + net-of-cost geometric fitness, property-tested `optimised == reference` over
//! seeded random inputs, plus a mutation guard that proves the oracle is non-vacuous.
//!
//! The references are written from scratch here — they do **not** call the incremental fold /
//! `apply_fill` / `split_windows` / `NoiseRobustFitness` / `log_growth` code they check. Independence is
//! structural: a plain re-derivation of the same contract, not a re-use of the same code. Test/dev-only:
//! no production or hot-path cost. Design note: `docs/architecture/qe-432-slow-reference-oracle-design.md`.

use qe_determinism::{derive_seed, seed_rng, DetRng};
use qe_domain::{Bar, Direction, Price, Qty, Resolution, Timestamp};
use qe_signal::{
    reconstruct_batch, CatalogueConfig, Clause, Decision, ExitParams, FeatureSchema, FeatureVector,
    Genome, PositionState, QState, RiskParams, RuleSet, CLAUSES_PER_SET, REP_VERSION,
};
use qe_wfo::backtest::{backtest, BacktestConfig, Bar as BtBar, DEFAULT_ADV_WINDOW};
use qe_wfo::friction::{FeeSchedule, FrictionConfig, SlippageModel};
use rand_core::RngCore;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::{Decimal, MathematicalOps};

/// Seeded randomised cases per property (documented in the design note). Two equivalence properties ⇒
/// ≥ 512 randomised equivalence cases per run, plus the two mutation-guard corpora.
const CASES: u64 = 256;

/// Master seed for the whole suite — fixed, so every run is byte-reproducible (determinism harness safe).
const MASTER_SEED: u64 = 0x5145_3433_3200_0001; // "QE432" tag

/// f64 tolerance for the independently-ordered float re-computations (per-bar returns, fitness mean).
/// IEEE-754 non-associativity permits last-ULP drift; `1e-9` is far tighter than any fitness gap the
/// search resolves (elite replacement works in units of standard error, ~1e-2..1e-4) yet absorbs
/// reordering noise. In practice the observed difference is 0.0 for most cases.
const F64_TOL: f64 = 1e-9;

// ---------------------------------------------------------------------------------------------------
// Small deterministic RNG helpers (over `qe_determinism::DetRng`, the project's portable ChaCha8 RNG).
// ---------------------------------------------------------------------------------------------------

/// Uniform integer in `[0, n)` (`n > 0`).
fn below(rng: &mut DetRng, n: u64) -> u64 {
    rng.next_u64() % n
}

/// A small positive `Decimal` with `scale` decimal places and mantissa in `[lo, hi)`.
fn dec(rng: &mut DetRng, lo: i64, hi: i64, scale: u32) -> Decimal {
    let span = (hi - lo).max(1) as u64;
    Decimal::new(lo + below(rng, span) as i64, scale)
}

// ===================================================================================================
// (a) Multi-resolution bar roll-up oracle
// ===================================================================================================

/// Which reconstruct bug (if any) the reference-shaped roll-up injects — used by the mutation guard.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RollBug {
    /// The correct, independent reference.
    None,
    /// A plausible fold typo: `volume = max(volume)` instead of `Σ volume`.
    VolumeMax,
}

/// Independent, deliberately naive roll-up: for each epoch-aligned window present in the input, do a
/// **fresh O(n) scan** of the whole base slice and aggregate its members. This is `O(n · windows)` — the
/// opposite of the optimised single-pass incremental fold, so it shares no code with it.
///
/// Inputs are strictly-ascending base bars (the documented, roamed domain), so each window is contiguous
/// and grouping-by-window-start agrees with the single-current-window fold.
fn reference_rollup(
    base_bars: &[Bar],
    base: Resolution,
    target: Resolution,
    bug: RollBug,
) -> Vec<Bar> {
    assert!(target.minutes() > base.minutes() && target.minutes().is_multiple_of(base.minutes()));
    let target_ms = i64::from(target.minutes()) * 60_000;
    let window_of = |b: &Bar| b.open_time().millis().div_euclid(target_ms) * target_ms;

    // Distinct window starts, in ascending order of first appearance (input is ascending).
    let mut starts: Vec<i64> = Vec::new();
    for b in base_bars {
        let w = window_of(b);
        if starts.last() != Some(&w) {
            starts.push(w);
        }
    }

    let mut out = Vec::with_capacity(starts.len());
    for &start in &starts {
        let members: Vec<&Bar> = base_bars.iter().filter(|b| window_of(b) == start).collect();
        let first = members[0];
        let last = members[members.len() - 1];
        let open = first.open();
        let close = last.close();
        let mut high = first.high();
        let mut low = first.low();
        let mut volume = Decimal::ZERO;
        let mut trades = 0u64;
        for m in &members {
            if m.high() > high {
                high = m.high();
            }
            if m.low() < low {
                low = m.low();
            }
            match bug {
                RollBug::None => volume += m.volume().get(),
                RollBug::VolumeMax => {
                    if m.volume().get() > volume {
                        volume = m.volume().get();
                    }
                }
            }
            trades += m.trades();
        }
        out.push(
            Bar::new(
                Timestamp::from_millis(start),
                target,
                open,
                high,
                low,
                close,
                Qty::new(volume).expect("qe-432 test: valid by construction"),
                trades,
            )
            .expect("qe-432 test: valid by construction"),
        );
    }
    out
}

/// Build a strictly-ascending random slice of valid M5 base bars (random gaps → missing bars / skipped
/// windows), plus a randomly chosen coarser target.
fn random_rollup_case(rng: &mut DetRng) -> (Vec<Bar>, Resolution) {
    let targets = [
        Resolution::M15,
        Resolution::M30,
        Resolution::H1,
        Resolution::H4,
        Resolution::H12,
        Resolution::D1,
    ];
    let target = targets[below(rng, targets.len() as u64) as usize];

    let n = 1 + below(rng, 60); // 1..=60 base bars
    let mut bars = Vec::with_capacity(n as usize);
    let mut slot: i64 = 0;
    for _ in 0..n {
        let open_ms = slot * 5 * 60_000;
        let low = dec(rng, 5_000, 15_000, 2); // ~50..150
        let span = dec(rng, 0, 2_000, 2); // 0..20
        let high = low + span;
        // open, close inside [low, high].
        let o_off = if span.is_zero() {
            Decimal::ZERO
        } else {
            dec(rng, 0, 2_000, 2).min(span)
        };
        let c_off = if span.is_zero() {
            Decimal::ZERO
        } else {
            dec(rng, 0, 2_000, 2).min(span)
        };
        let open = low + o_off;
        let close = low + c_off;
        let volume = dec(rng, 0, 100_000, 3); // 0..100
        let trades = below(rng, 200);
        bars.push(
            Bar::new(
                Timestamp::from_millis(open_ms),
                Resolution::M5,
                Price::new(open).expect("qe-432 test: valid by construction"),
                Price::new(high).expect("qe-432 test: valid by construction"),
                Price::new(low).expect("qe-432 test: valid by construction"),
                Price::new(close).expect("qe-432 test: valid by construction"),
                Qty::new(volume).expect("qe-432 test: valid by construction"),
                trades,
            )
            .expect("qe-432 test: valid by construction"),
        );
        slot += 1 + below(rng, 6) as i64; // 1..=6 slot gap
    }
    (bars, target)
}

#[test]
fn reconstruct_oracle_matches_over_seeded_random_cases() {
    for i in 0..CASES {
        let mut rng = seed_rng(derive_seed(MASTER_SEED, i));
        let (bars, target) = random_rollup_case(&mut rng);

        let optimised = reconstruct_batch(&bars, Resolution::M5, target)
            .expect("qe-432 test: valid by construction");
        let reference = reference_rollup(&bars, Resolution::M5, target, RollBug::None);

        assert_eq!(
            optimised, reference,
            "roll-up oracle mismatch on case {i} (target {target}): optimised != independent reference"
        );
    }
}

#[test]
fn reconstruct_oracle_is_non_vacuous_mutation_guard() {
    // The oracle must distinguish a correct optimised path from a bugged one. On a corpus where the
    // reference tracks the real optimised path exactly, a volume-fold bug (max instead of sum) must be
    // caught by the reference on at least one case — otherwise the oracle would be vacuous.
    let mut caught = false;
    for i in 0..CASES {
        let mut rng = seed_rng(derive_seed(MASTER_SEED ^ 0xAA, i));
        let (bars, target) = random_rollup_case(&mut rng);

        let optimised = reconstruct_batch(&bars, Resolution::M5, target)
            .expect("qe-432 test: valid by construction");
        let reference = reference_rollup(&bars, Resolution::M5, target, RollBug::None);
        let mutant = reference_rollup(&bars, Resolution::M5, target, RollBug::VolumeMax);

        assert_eq!(
            optimised, reference,
            "reference must track the real optimised path (case {i})"
        );
        if reference != mutant {
            caught = true;
        }
    }
    assert!(
        caught,
        "mutation guard vacuous: the volume-fold bug was never caught by the reference"
    );
}

// ===================================================================================================
// (b) Net-of-cost cost-ledger + geometric fitness oracle
// ===================================================================================================

/// Which cost-ledger bug (if any) the reference backtest injects — used by the mutation guard.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CostBug {
    /// The correct, independent reference.
    None,
    /// Drop the `cost_multiplier` on slippage — a plausible net-of-cost regression (exactly the
    /// systematic per-trade cost bias the Deflated Sharpe cannot remove).
    DropSlipMultiplier,
}

/// The subset of a backtest outcome the oracle checks.
struct RefOut {
    returns: Vec<f64>,
    trades: usize,
    net_pnl: Decimal,
    funding: Decimal,
    accepted: bool,
    fitness_mean: f64,
}

const BPS_DENOMINATOR: i64 = 10_000;

/// Independent, dead-simple trade-by-trade replay of the cost-ledger + net-of-cost geometric fitness.
///
/// Reuses `Genome::decide` (the shared signal-layer decision primitive is *not* part of the cost path
/// under test) but recomputes the exact-`Decimal` cash/mark ledger, funding, per-bar net returns,
/// `net_pnl`, and the windowed `mean ln(1+r)` fitness from scratch — never calling the crate's
/// `apply_fill` / `split_windows` / `NoiseRobustFitness` / `log_growth`.
fn reference_backtest(
    genome: &Genome,
    bars: &[BtBar],
    cfg: &BacktestConfig,
    bug: CostBug,
) -> RefOut {
    let size_frac = Decimal::from(genome.risk.size_bps) / Decimal::from(BPS_DENOMINATOR);
    let m = cfg.friction.cost_multiplier;
    let taker = cfg.friction.fees.taker;
    let slippage = cfg.friction.slippage;

    let mut cash = Decimal::ONE;
    let mut pos = Decimal::ZERO;
    let mut equity_prev = Decimal::ONE;
    let mut entry_bar: Option<usize> = None;
    let mut pending: Option<Decision> = None;
    let mut returns: Vec<f64> = Vec::new();
    let mut trades = 0usize;
    let mut funding = Decimal::ZERO;

    // Independent rolling-ADV window (QE-440), mirroring the optimised path's trailing mean bar volume.
    let mut adv_vols: std::collections::VecDeque<Decimal> = std::collections::VecDeque::new();
    let mut adv_sum = Decimal::ZERO;

    for (i, bar) in bars.iter().enumerate() {
        let price = bar.price;

        // Roll the ADV window forward with this bar's volume before pricing any fill.
        if adv_vols.len() == DEFAULT_ADV_WINDOW {
            if let Some(oldest) = adv_vols.pop_front() {
                adv_sum -= oldest;
            }
        }
        adv_vols.push_back(bar.volume);
        adv_sum += bar.volume;
        let adv = adv_sum / Decimal::from(adv_vols.len());

        // (1) Fill the order scheduled at the previous bar, at this bar's price.
        if let Some(order) = pending.take() {
            if price > Decimal::ZERO {
                match order {
                    Decision::Enter(dir) => {
                        let notional = size_frac * equity_prev;
                        let qty = notional / price;
                        if qty > Decimal::ZERO {
                            let signed = match dir {
                                Direction::Long => qty,
                                Direction::Short => -qty,
                            };
                            apply(
                                &mut cash, &mut pos, signed, price, taker, &slippage, adv, m, bug,
                            );
                            entry_bar = Some(i);
                            trades += 1;
                        }
                    }
                    Decision::Exit => {
                        let qty = pos.abs();
                        if qty > Decimal::ZERO {
                            let signed = if pos > Decimal::ZERO { -qty } else { qty };
                            apply(
                                &mut cash, &mut pos, signed, price, taker, &slippage, adv, m, bug,
                            );
                            entry_bar = None;
                        }
                    }
                    Decision::Hold => {}
                }
            }
        }

        // (2) Funding against the held position (longs pay shorts when rate > 0).
        if let Some(rate) = bar.funding_rate {
            let flow = -pos * price * rate;
            cash += flow;
            funding += flow;
        }

        // (3) Mark equity and record the net-of-cost per-bar return.
        let equity = cash + pos * price;
        if i > 0 {
            let r = if equity_prev.is_zero() {
                -1.0
            } else {
                (equity / equity_prev - Decimal::ONE)
                    .to_f64()
                    .unwrap_or(0.0)
            };
            returns.push(r);
        }
        equity_prev = equity;

        // (4) Decide the next bar's order (fills at i+1) — shared decision primitive, cost-blind.
        let position = match (pos, entry_bar) {
            (q, Some(j)) if q > Decimal::ZERO => {
                PositionState::held(Direction::Long, (i - j) as u16)
            }
            (q, Some(j)) if q < Decimal::ZERO => {
                PositionState::held(Direction::Short, (i - j) as u16)
            }
            _ => PositionState::flat(),
        };
        pending = Some(genome.decide(&bar.features, position));
    }

    let net_pnl = equity_prev - Decimal::ONE;
    let accepted = trades >= cfg.min_trades && !returns.is_empty();
    let fitness_mean = if accepted {
        windowed_log_growth_mean(&returns, cfg.windows)
    } else {
        f64::NEG_INFINITY
    };

    RefOut {
        returns,
        trades,
        net_pnl,
        funding,
        accepted,
        fitness_mean,
    }
}

/// Apply one fill to the exact-`Decimal` cash/mark ledger. Costs reduce cash, so returns are
/// net-of-cost. `signed` is the signed filled quantity (`+` buy / `−` sell).
#[allow(clippy::too_many_arguments)] // a fill is genuinely this many independent inputs
fn apply(
    cash: &mut Decimal,
    pos: &mut Decimal,
    signed: Decimal,
    price: Decimal,
    taker: Decimal,
    slippage: &SlippageModel,
    adv: Decimal,
    mult: Decimal,
    bug: CostBug,
) {
    let qty_abs = signed.abs();
    let notional_abs = (qty_abs * price).abs();
    let fee = notional_abs * taker * mult;
    // Independent re-derivation of the QE-440 concave participation impact: `notional·(half_spread +
    // impact_coeff·(qty/adv)^β)`. Recomputed from scratch (not via `SlippageModel::cost`).
    let participation = if adv > Decimal::ZERO {
        qty_abs / adv
    } else {
        Decimal::ZERO
    };
    let impact = if participation > Decimal::ZERO {
        slippage.impact_coeff * participation.powd(slippage.impact_exponent)
    } else {
        Decimal::ZERO
    };
    let slip_base = notional_abs * (slippage.half_spread + impact);
    let slip = match bug {
        CostBug::None => slip_base * mult,
        CostBug::DropSlipMultiplier => slip_base, // injected bug: forgets the cost multiplier
    };
    *cash -= signed * price;
    *cash -= fee + slip;
    *pos += signed;
}

/// Independent windowed geometric fitness: split `returns` into up to `k` contiguous balanced
/// sub-windows, take `mean_i ln(1+r_i)` per window (`−∞` if any `r ≤ −1`), and average the windows.
fn windowed_log_growth_mean(returns: &[f64], k: usize) -> f64 {
    if returns.is_empty() {
        return 0.0;
    }
    let k = k.max(1).min(returns.len());
    let base = returns.len() / k;
    let rem = returns.len() % k;
    let mut idx = 0usize;
    let mut sum_growth = 0.0;
    let mut windows = 0usize;
    for w in 0..k {
        let size = base + usize::from(w < rem);
        if size == 0 {
            continue;
        }
        let slice = &returns[idx..idx + size];
        idx += size;
        windows += 1;
        // Per-window log-growth.
        let mut sum_log = 0.0;
        let mut ruined = false;
        for &r in slice {
            let g = 1.0 + r;
            if g <= 0.0 {
                ruined = true;
                break;
            }
            sum_log += g.ln();
        }
        if ruined {
            return f64::NEG_INFINITY;
        }
        sum_growth += sum_log / slice.len() as f64;
    }
    sum_growth / windows as f64
}

fn schema() -> FeatureSchema {
    FeatureSchema::from_catalogue(&CatalogueConfig { states: 5 })
}

/// A random valid genome (repaired onto the validity manifold).
fn random_genome(rng: &mut DetRng, s: &FeatureSchema) -> Genome {
    let bank = |rng: &mut DetRng| {
        let mut clauses = [Clause {
            enabled: false,
            feature: 0,
            lo: 0,
            hi: 0,
        }; CLAUSES_PER_SET];
        for c in &mut clauses {
            c.enabled = below(rng, 2) == 1;
            c.feature = below(rng, s.len() as u64) as u16;
            let a = below(rng, s.num_states() as u64) as u16;
            let b = below(rng, s.num_states() as u64) as u16;
            c.lo = a.min(b);
            c.hi = a.max(b);
        }
        RuleSet {
            clauses,
            min_satisfied: (1 + below(rng, CLAUSES_PER_SET as u64)) as u8,
        }
    };
    let mut g = Genome {
        version: REP_VERSION,
        long_entry: bank(rng),
        short_entry: bank(rng),
        exit: ExitParams {
            max_holding_bars: (1 + below(rng, 8)) as u16,
            exit_on_opposite: below(rng, 2) == 1,
        },
        risk: RiskParams {
            size_bps: (1 + below(rng, 10_000)) as u16,
        },
    };
    g.repair(s);
    g
}

/// A random bar series: random positive prices, sparse warm feature states, occasional funding stamps.
fn random_bars(rng: &mut DetRng, s: &FeatureSchema) -> Vec<BtBar> {
    let n = 40 + below(rng, 160); // 40..=199 bars
    (0..n)
        .map(|i| {
            let mut states = vec![None; s.len()];
            // Warm a random subset of feature slots.
            for slot in states.iter_mut() {
                if below(rng, 2) == 1 {
                    *slot = Some(QState::from_index(below(rng, s.num_states() as u64) as u16));
                }
            }
            let price = dec(rng, 1_000, 40_000, 2); // ~10..400, always > 0
            let funding_rate = if below(rng, 4) == 0 {
                Some(Decimal::new(below(rng, 20) as i64 - 10, 4)) // −0.0010..0.0009
            } else {
                None
            };
            let volume = dec(rng, 1, 100_000, 3); // ~0.001..100 contracts, always > 0
            BtBar {
                features: FeatureVector {
                    time_ms: i as i64 * 60_000,
                    states,
                },
                price,
                volume,
                funding_rate,
            }
        })
        .collect()
}

/// A random friction config with `cost_multiplier` forced into `mult_lo..mult_hi` (so the mutation guard
/// can exercise a multiplier ≠ 1).
fn random_friction(rng: &mut DetRng, mult_lo: i64, mult_hi: i64) -> FrictionConfig {
    FrictionConfig {
        fees: FeeSchedule {
            taker: dec(rng, 1, 10, 4),
            maker: dec(rng, 1, 5, 4),
        },
        slippage: SlippageModel {
            half_spread: dec(rng, 0, 5, 4),
            impact_coeff: dec(rng, 0, 5, 2), // 0..0.05 participation coefficient
            impact_exponent: dec(rng, 2, 6, 1), // β ∈ [0.2, 0.6)
        },
        cost_multiplier: Decimal::from(
            mult_lo + below(rng, (mult_hi - mult_lo).max(1) as u64) as i64,
        ),
    }
}

fn random_backtest_cfg(rng: &mut DetRng, mult_lo: i64, mult_hi: i64) -> BacktestConfig {
    BacktestConfig {
        friction: random_friction(rng, mult_lo, mult_hi),
        min_trades: 1 + below(rng, 12) as usize,
        windows: 1 + below(rng, 6) as usize,
    }
}

/// Compare the two f64 fitness/return computations: `−∞` is an exact category, finite within `F64_TOL`.
fn f64_agree(a: f64, b: f64) -> bool {
    if a.is_infinite() || b.is_infinite() {
        return a.is_infinite() && b.is_infinite() && a.is_sign_negative() == b.is_sign_negative();
    }
    (a - b).abs() <= F64_TOL
}

#[test]
fn net_of_cost_fitness_oracle_matches_over_seeded_random_cases() {
    let s = schema();
    for i in 0..CASES {
        let mut rng = seed_rng(derive_seed(MASTER_SEED ^ 0x55, i));
        let g = random_genome(&mut rng, &s);
        let bars = random_bars(&mut rng, &s);
        let cfg = random_backtest_cfg(&mut rng, 1, 20);

        let opt = backtest(&g, &bars, &cfg);
        let re = reference_backtest(&g, &bars, &cfg, CostBug::None);

        // Exact-decimal money and integer counts must be byte-identical.
        assert_eq!(opt.trades, re.trades, "trade count mismatch on case {i}");
        assert_eq!(opt.net_pnl, re.net_pnl, "net_pnl mismatch on case {i}");
        assert_eq!(opt.funding, re.funding, "funding mismatch on case {i}");
        assert_eq!(opt.accepted, re.accepted, "accepted mismatch on case {i}");

        // Independently-ordered float re-computations agree within the documented tolerance.
        assert_eq!(
            opt.returns.len(),
            re.returns.len(),
            "returns length mismatch on case {i}"
        );
        for (k, (a, b)) in opt.returns.iter().zip(re.returns.iter()).enumerate() {
            assert!(
                f64_agree(*a, *b),
                "return[{k}] mismatch on case {i}: {a} vs {b}"
            );
        }
        assert!(
            f64_agree(opt.fitness.mean, re.fitness_mean),
            "fitness mean mismatch on case {i}: {} vs {}",
            opt.fitness.mean,
            re.fitness_mean
        );
    }
}

#[test]
fn net_of_cost_oracle_is_non_vacuous_mutation_guard() {
    // On a corpus where the reference tracks the real optimised path exactly (cost_multiplier ≥ 2 and
    // some slippage), dropping the cost multiplier on slippage must move net_pnl — caught by the oracle
    // on at least one case, proving it is non-vacuous.
    let s = schema();
    let mut caught = false;
    for i in 0..CASES {
        let mut rng = seed_rng(derive_seed(MASTER_SEED ^ 0xF0, i));
        let g = random_genome(&mut rng, &s);
        let bars = random_bars(&mut rng, &s);
        let cfg = random_backtest_cfg(&mut rng, 2, 30); // multiplier ≥ 2 so the bug bites

        let opt = backtest(&g, &bars, &cfg);
        let re = reference_backtest(&g, &bars, &cfg, CostBug::None);
        let mutant = reference_backtest(&g, &bars, &cfg, CostBug::DropSlipMultiplier);

        // Reference tracks the real optimised path.
        assert_eq!(
            opt.net_pnl, re.net_pnl,
            "reference must track the real optimised path (case {i})"
        );
        if re.net_pnl != mutant.net_pnl {
            caught = true;
        }
    }
    assert!(
        caught,
        "mutation guard vacuous: dropping the cost multiplier was never caught by the oracle"
    );
}
