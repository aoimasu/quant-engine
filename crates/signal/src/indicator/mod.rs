//! The shared indicator catalogue (QE-107): a broad set of **quantised**, **finite-lookback**,
//! **batch/streaming-identical** indicators — the substrate the strategy genome reasons over.
//!
//! Design (see the QE-107 design note):
//! - **One [`Indicator::update`] path** drives both batch ([`compute_batch`]) and streaming, so they
//!   are identical by construction (AC #1).
//! - **Finite-window (FIR) kernels**: each indicator's latest output reads exactly the last
//!   `lookback` samples and nothing older, so declared lookback == data dependency (AC #2) — the
//!   property purge/embargo needs.
//! - **Point-wise quantisation** ([`Quantiser`]): no rolling quantiles / dataset fit, so the
//!   discrete state never peeks at future data.

mod quant;
mod roll;

mod flow;
mod price;

/// QE-451 Phase 0 — the `Expr`/`Kernel` tree-interpreter seam proof (default-off; nothing in the
/// default pipeline references it). See the module docs and
/// `docs/architecture/qe-451-phase0-expr-seam-design.md`.
pub mod expr;

pub use quant::{QState, Quantiser};

use expr::Expr;

use rust_decimal::Decimal;

use qe_domain::Bar;

use roll::Roll;

/// The version of the catalogue. Bump when the indicator set or any indicator's semantics change.
pub const CATALOGUE_VERSION: u32 = 1;

/// One time-step of input: the base bar plus the optional aligned scalar context (funding rate,
/// open interest, premium) the flow factors read.
#[derive(Debug, Clone)]
pub struct Sample {
    /// The base-resolution bar.
    pub bar: Bar,
    /// Funding rate at this step, if known.
    pub funding: Option<Decimal>,
    /// Open interest at this step, if known.
    pub open_interest: Option<Decimal>,
    /// Premium (perp − underlier) at this step, if known.
    pub premium: Option<Decimal>,
}

impl Sample {
    /// A bar-only sample (no scalar context).
    #[must_use]
    pub fn from_bar(bar: Bar) -> Self {
        Sample {
            bar,
            funding: None,
            open_interest: None,
            premium: None,
        }
    }
}

/// What an indicator declares about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndicatorSpec {
    /// Stable identifier, e.g. `"rsi_14"`.
    pub id: String,
    /// Max lookback — the exact number of most-recent **observed inputs** the latest output depends
    /// on (feeds purge/embargo).
    ///
    /// **Units caveat (leakage-relevant).** For price/volume indicators this is in *bars*, so the
    /// time-axis dependency is exactly `lookback × bar_interval`. For the **flow factors** it counts
    /// the last `lookback` *present* scalars (funding/OI/premium), because a step whose scalar is
    /// absent is skipped (see [`flow`]). If those scalars arrive **sparser than the bar grid** (e.g.
    /// funding posts every 8h against 5m bars), the true *time-axis* dependency spans many more bars
    /// than `lookback`. The downstream contract is therefore: **QE-108 must feed dense, bar-aligned
    /// scalars** (forward-filled to the grid) so flow `lookback` is in bar units like the rest, **or**
    /// **QE-128 must size the embargo for the coarsest flow-factor cadence**. With dense input
    /// (as the AC tests use) `lookback` is exact for every indicator.
    pub lookback: usize,
    /// Number of discrete states the quantised output takes.
    pub num_states: u16,
}

/// A quantised, finite-lookback indicator usable identically in batch and streaming.
pub trait Indicator {
    /// This indicator's declared spec.
    fn spec(&self) -> IndicatorSpec;
    /// Feed one sample (in time order); returns the quantised state once warmed up, else `None`.
    fn update(&mut self, sample: &Sample) -> Option<QState>;
    /// Reset to the pre-warmup state.
    fn reset(&mut self);
}

/// Internal finite-window kernel: implementors get [`Indicator`] for free via the blanket impl
/// below, so each indicator only writes its observe/warm/value logic.
trait Kernel {
    fn id(&self) -> String;
    fn lookback(&self) -> usize;
    fn quantiser(&self) -> &Quantiser;
    /// Push this sample's relevant fields into the kernel's rolling windows.
    fn observe(&mut self, sample: &Sample);
    /// Whether enough samples have been observed to produce a value.
    fn warm(&self) -> bool;
    /// The continuous value from the current window (pre-quantisation); `None` if undefined.
    fn raw(&self) -> Option<Decimal>;
    /// Reset all windows.
    fn clear(&mut self);
}

impl<K: Kernel> Indicator for K {
    fn spec(&self) -> IndicatorSpec {
        IndicatorSpec {
            id: self.id(),
            lookback: self.lookback(),
            num_states: self.quantiser().states(),
        }
    }

    fn update(&mut self, sample: &Sample) -> Option<QState> {
        self.observe(sample);
        if self.warm() {
            self.raw().map(|v| self.quantiser().quantise(v))
        } else {
            None
        }
    }

    fn reset(&mut self) {
        self.clear();
    }
}

/// Run an indicator over a whole sample slice — literally the streaming `update` loop, so batch and
/// streaming produce identical output (AC #1).
#[must_use]
pub fn compute_batch(indicator: &mut dyn Indicator, samples: &[Sample]) -> Vec<Option<QState>> {
    samples.iter().map(|s| indicator.update(s)).collect()
}

/// A qe-signal-native compiled-formula spec (QE-499 Phase A): an injected `Expr`-backed indicator
/// identified by its 64-hex `formula_hash`, plus the point-wise [`Quantiser`] it is scored through.
///
/// It carries the *un*compiled [`Expr`] (compilation into a live [`Box<dyn Indicator>`] happens inside
/// [`catalogue`], since `Box<dyn Indicator>` is not `Clone`), so `CompiledFormula` stays `Clone` and
/// `CatalogueConfig` can own a `Vec` of them. This type is deliberately qe-signal-native: the
/// `FormulaPool → CompiledFormula` mapping (which would need `qe-formula-pool`) is **deferred to qe-cli
/// in Phase B**, so qe-signal never gains a `qe-formula-pool` edge.
#[derive(Debug, Clone)]
pub struct CompiledFormula {
    /// Deterministic id — the 64-hex SHA-256 `formula_hash` over the tree's canonical S-expression.
    pub id: String,
    /// The FIR expression tree to compile (via [`expr::compile`]).
    pub expr: Expr,
    /// The point-wise quantiser this formula's continuous value is bucketed through.
    pub quantiser: Quantiser,
}

/// Configuration for building the catalogue. `states` sets the (configurable) number of quantised
/// states every indicator uses.
///
/// `formula_pool` (QE-499 Phase A) is the **injected** set of evolved formulas appended to the
/// catalogue at a fixed position after the price + flow kernels. It is **empty by default** and no
/// train/CLI path populates it in Phase A — a non-empty pool is reachable only from unit tests — so
/// the default catalogue, its [`FeatureSchema`](crate::feature::FeatureSchema), and its
/// [`CatalogueIdentity`](crate::feature::CatalogueIdentity) are byte-identical to the pre-QE-499 build.
#[derive(Debug, Clone)]
pub struct CatalogueConfig {
    /// Number of discrete states per indicator (≥ 2).
    pub states: u16,
    /// Injected evolved formulas (QE-499 Phase A). Empty by default; compiled inside [`catalogue`].
    pub formula_pool: Vec<CompiledFormula>,
}

impl Default for CatalogueConfig {
    fn default() -> Self {
        CatalogueConfig {
            states: 5,
            formula_pool: Vec::new(),
        }
    }
}

impl CatalogueConfig {
    fn states(&self) -> u16 {
        self.states.max(2)
    }
}

/// Build the full indicator catalogue with `cfg` applied (configurable state count).
///
/// The set spans moving-average ratios, momentum/returns, oscillators (RSI, Stochastic, Williams %R,
/// CCI, MFI, Aroon), volatility (ATR%, Bollinger %B/bandwidth, std-returns), volume (volume-ratio,
/// signed-volume, CMF), MACD, and the funding/OI/premium flow factors — ≥ 20 indicators.
#[must_use]
pub fn catalogue(cfg: &CatalogueConfig) -> Vec<Box<dyn Indicator>> {
    let s = cfg.states();
    let mut v: Vec<Box<dyn Indicator>> = Vec::new();
    price::extend_catalogue(&mut v, s);
    flow::extend_catalogue(&mut v, s);
    // QE-499 Phase A: append the injected formula pool at a **fixed position after** the price + flow
    // kernels, in **sorted-by-id order** (id == 64-hex `formula_hash`), so the catalogue — and thus its
    // `FeatureSchema`/`CatalogueIdentity` — is a pure, deterministic function of the (sorted) pool. Each
    // formula is compiled here via the existing `expr::compile` (its `Expr` is not turned into a live
    // indicator until now, keeping `CompiledFormula` `Clone`). The pool is **empty by default** and no
    // Phase-A caller populates it, so this loop is a no-op and the default catalogue is byte-identical.
    let mut injected: Vec<&CompiledFormula> = cfg.formula_pool.iter().collect();
    injected.sort_by(|a, b| a.id.cmp(&b.id));
    for f in injected {
        v.push(expr::compile(&f.id, &f.expr, f.quantiser.clone()));
    }
    v
}

/// The maximum declared lookback across the catalogue (feeds purge/embargo sizing).
#[must_use]
pub fn max_lookback(cfg: &CatalogueConfig) -> usize {
    catalogue(cfg)
        .iter()
        .map(|i| i.spec().lookback)
        .max()
        .unwrap_or(0)
}

/// Shared rolling view of the last `cap` bars' fields (only the ones a kernel reads are consulted).
#[derive(Debug, Clone)]
struct Bars {
    close: Roll,
    high: Roll,
    low: Roll,
    volume: Roll,
    typical: Roll,
}

impl Bars {
    fn new(cap: usize) -> Self {
        Bars {
            close: Roll::new(cap),
            high: Roll::new(cap),
            low: Roll::new(cap),
            volume: Roll::new(cap),
            typical: Roll::new(cap),
        }
    }

    fn observe(&mut self, bar: &Bar) {
        let (h, l, c) = (bar.high().get(), bar.low().get(), bar.close().get());
        self.close.push(c);
        self.high.push(h);
        self.low.push(l);
        self.volume.push(bar.volume().get());
        self.typical.push((h + l + c) / Decimal::from(3));
    }

    fn is_full(&self) -> bool {
        self.close.is_full()
    }

    fn clear(&mut self) {
        let cap = self.close.cap();
        *self = Bars::new(cap);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qe_domain::{Price, Qty, Resolution, Timestamp};

    const MIN: i64 = 60_000;

    fn dec(n: i64) -> Decimal {
        Decimal::from(n)
    }

    /// A deterministic but varied sample series of length `n`, with all scalar context present so
    /// every indicator (incl. flow factors) warms up.
    fn series(n: usize) -> Vec<Sample> {
        (0..n)
            .map(|i| {
                let i64i = i as i64;
                // A gently oscillating price so highs/lows/closes differ.
                let base = 100 + (i64i % 7) * 3 - (i64i % 3) * 2 + i64i / 5;
                let high = base + 5 + (i64i % 4);
                let low = base - 5 - (i64i % 5);
                let close = base + (i64i % 3) - 1;
                let bar = Bar::new(
                    Timestamp::from_millis(i64i * 5 * MIN),
                    Resolution::M5,
                    Price::new(dec(base)).unwrap(),
                    Price::new(dec(high)).unwrap(),
                    Price::new(dec(low)).unwrap(),
                    Price::new(dec(close)).unwrap(),
                    Qty::new(dec(10 + (i64i % 6))).unwrap(),
                    1 + (i % 4) as u64,
                )
                .unwrap();
                Sample {
                    bar,
                    funding: Some(Decimal::new((i64i % 5) - 2, 4)), // small ± funding
                    open_interest: Some(dec(1000 + i64i * 7)),
                    premium: Some(Decimal::new((i64i % 3) - 1, 4)),
                }
            })
            .collect()
    }

    #[test]
    fn catalogue_has_at_least_twenty_indicators_with_unique_ids() {
        let cat = catalogue(&CatalogueConfig::default());
        assert!(cat.len() >= 20, "catalogue has {} indicators", cat.len());
        let mut ids: Vec<String> = cat.iter().map(|i| i.spec().id).collect();
        let total = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), total, "indicator ids must be unique");
    }

    #[test]
    fn every_indicator_respects_configured_state_count() {
        let cfg = CatalogueConfig {
            states: 7,
            formula_pool: Vec::new(),
        };
        for ind in catalogue(&cfg) {
            assert_eq!(ind.spec().num_states, 7, "{}", ind.spec().id);
        }
    }

    #[test]
    fn qe499_empty_pool_byte_identical_nonempty_pool_changes_identity_and_is_causal() {
        // QE-499 Phase A acceptance: the EMPTY pool leaves the catalogue / schema / identity exactly as
        // today (golden-safe), while a synthetic NON-EMPTY pool changes `CatalogueIdentity` in BOTH
        // `id_hash` (the injected id rides the ordered catalogue) and the dedicated `formula_pool` slot,
        // and injects a real non-constant, causal feature.
        use crate::feature::{CatalogueIdentity, FeatureSchema};
        use expr::{ExprTree, Field, WinOp};

        let default_cfg = CatalogueConfig::default();

        // --- EMPTY pool: byte-identical to today. ---
        let base_ids: Vec<String> = catalogue(&default_cfg)
            .iter()
            .map(|i| i.spec().id)
            .collect();
        let base_identity = CatalogueIdentity::from_config(&default_cfg);
        assert_eq!(
            base_identity,
            CatalogueIdentity::current(),
            "empty-pool identity moved — a golden would move"
        );
        assert!(base_identity.formula_pool.is_empty());
        // The default identity still OMITS the pool field from JSON (skip_serializing_if preserved).
        let json = serde_json::to_string(&base_identity).unwrap();
        assert!(
            !json.contains("formula_pool"),
            "empty-pool identity must not serialise the pool field: {json}"
        );
        assert_eq!(
            FeatureSchema::from_catalogue(&default_cfg).len(),
            base_ids.len()
        );

        // --- NON-EMPTY pool: one real causal formula, id == its 64-hex formula_hash. ---
        let tree = ExprTree::repaired(Expr::Window(
            WinOp::Zscore,
            Box::new(Expr::Input(Field::Close)),
            10,
        ));
        let formula_hash = tree.canonical_hash();
        assert_eq!(formula_hash.len(), 64, "id must be a 64-hex formula_hash");
        let formula = CompiledFormula {
            id: formula_hash.clone(),
            expr: tree.canonical(),
            quantiser: Quantiser::Linear {
                min: dec(-4),
                max: dec(4),
                states: default_cfg.states,
            },
        };
        let pooled_cfg = CatalogueConfig {
            states: default_cfg.states,
            formula_pool: vec![formula],
        };

        // The injected indicator is appended after price+flow, with id == formula_hash.
        let pooled: Vec<Box<dyn Indicator>> = catalogue(&pooled_cfg);
        let pooled_ids: Vec<String> = pooled.iter().map(|i| i.spec().id).collect();
        assert_eq!(
            pooled_ids.len(),
            base_ids.len() + 1,
            "catalogue must grow by one"
        );
        assert_eq!(
            pooled_ids.last().unwrap(),
            &formula_hash,
            "injected id must be the formula_hash, appended last"
        );

        // Identity changes in BOTH id_hash and formula_pool, and is a pure function of the sorted pool.
        let pooled_identity = CatalogueIdentity::from_config(&pooled_cfg);
        assert_ne!(pooled_identity, base_identity);
        assert_ne!(pooled_identity.id_hash, base_identity.id_hash);
        assert_eq!(pooled_identity.formula_pool, vec![formula_hash.clone()]);
        assert!(serde_json::to_string(&pooled_identity)
            .unwrap()
            .contains("formula_pool"));

        // The injected feature is CAUSAL (warms exactly at its lookback) and NON-CONSTANT over the
        // shared series.
        let samples = series(120);
        let mut inj = catalogue(&pooled_cfg).pop().unwrap();
        let lookback = inj.spec().lookback;
        let out = compute_batch(inj.as_mut(), &samples);
        assert!(
            out[..lookback - 1].iter().all(Option::is_none),
            "injected feature emitted before its lookback"
        );
        assert!(
            out[lookback - 1].is_some(),
            "injected feature did not warm at lookback"
        );
        let states: Vec<u16> = out.iter().flatten().map(|q| q.index()).collect();
        assert!(
            states.iter().any(|&s| s != states[0]),
            "injected feature is constant — not a useful feature"
        );
    }

    #[test]
    fn ac1_batch_equals_streaming_for_every_indicator() {
        let cfg = CatalogueConfig::default();
        let samples = series(80);
        for mut ind in catalogue(&cfg) {
            // Batch.
            let batch = compute_batch(ind.as_mut(), &samples);
            // Streaming: fresh indicator, fed one at a time.
            ind.reset();
            let streamed: Vec<_> = samples.iter().map(|s| ind.update(s)).collect();
            assert_eq!(batch, streamed, "batch≠streaming for {}", ind.spec().id);
        }
    }

    #[test]
    fn ac2_warmup_emits_none_until_exactly_lookback_then_some() {
        // Proves each indicator consumes at least `lookback` samples before its first output (the
        // ⊇ direction of "lookback == dependency").
        let cfg = CatalogueConfig::default();
        let samples = series(120);
        for mut ind in catalogue(&cfg) {
            let lookback = ind.spec().lookback;
            let id = ind.spec().id;
            for (i, s) in samples.iter().enumerate() {
                let out = ind.update(s);
                if i + 1 < lookback {
                    assert!(out.is_none(), "{id} emitted before lookback at i={i}");
                } else if i + 1 == lookback {
                    assert!(out.is_some(), "{id} did not emit at exactly lookback");
                    break;
                }
            }
        }
    }

    #[test]
    fn ac2_latest_output_independent_of_out_of_window_samples() {
        // Proves the ⊆ direction: perturbing any sample strictly older than `lookback` leaves the
        // latest output byte-identical — the leakage-safety property purge/embargo relies on.
        let cfg = CatalogueConfig::default();
        let base = series(120);

        for mut ind in catalogue(&cfg) {
            let lookback = ind.spec().lookback;
            let id = ind.spec().id;

            let original = compute_batch(ind.as_mut(), &base);
            let last_state = *original.last().unwrap();

            // Perturb a sample just outside the latest window: index = len-1-lookback.
            let mut perturbed = base.clone();
            let idx = perturbed.len() - 1 - lookback;
            perturbed[idx] = perturb(&perturbed[idx]);

            ind.reset();
            let after = compute_batch(ind.as_mut(), &perturbed);
            assert_eq!(
                *after.last().unwrap(),
                last_state,
                "{id}: latest output changed by an out-of-window sample"
            );
        }
    }

    /// Replace every field of a sample with markedly different values.
    fn perturb(s: &Sample) -> Sample {
        let b = &s.bar;
        let bump = |p: Price| Price::new(p.get() + dec(50)).unwrap();
        let bar = Bar::new(
            b.open_time(),
            b.resolution(),
            bump(b.open()),
            bump(b.high()),
            bump(b.low()),
            bump(b.close()),
            Qty::new(b.volume().get() + dec(100)).unwrap(),
            b.trades() + 7,
        )
        .unwrap();
        Sample {
            bar,
            funding: Some(dec(1)),
            open_interest: Some(dec(999_999)),
            premium: Some(dec(1)),
        }
    }
}
