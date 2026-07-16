//! Live factor join — align live scalar context onto base bars and drive the shared catalogue (QE-206).
//!
//! A factor row (`qe_signal::FeatureVector`) is the per-bar vector of quantised catalogue states. Offline,
//! it is assembled from a [`Sample`](qe_signal::Sample) = base bar + as-of-aligned scalar context
//! (funding / open interest / premium). [`LiveFactorJoin`] performs that **as-of join on the live path**:
//! it keeps the last-known context value (a streaming as-of join over the time-ordered event flow) and, on
//! each incoming base bar, pairs the snapshot with the bar and drives the **unmodified**
//! [`FeatureAssembler`](qe_signal::FeatureAssembler). So a live factor row equals the offline feature
//! vector for the same sample, bar-for-bar — the parity guarantee (AC), proven against an independent
//! batch as-of join in the tests.

use rust_decimal::Decimal;

use qe_domain::Bar;
use qe_signal::{CatalogueConfig, FeatureAssembler, FeatureSchema, FeatureVector, Sample};

/// Joins live scalar context onto base bars and assembles factor rows via the shared catalogue.
///
/// Context observations (funding / open interest / premium) and base bars are fed in timestamp order; each
/// context observation updates the as-of snapshot, and each bar is paired with the snapshot in force —
/// exactly the most-recent value with `ts <= bar.open_time`, matching an offline batch as-of join.
pub struct LiveFactorJoin {
    assembler: FeatureAssembler,
    funding: Option<Decimal>,
    open_interest: Option<Decimal>,
    premium: Option<Decimal>,
}

impl LiveFactorJoin {
    /// A join driving the catalogue configured by `cfg`.
    #[must_use]
    pub fn new(cfg: &CatalogueConfig) -> Self {
        Self {
            assembler: FeatureAssembler::new(cfg),
            funding: None,
            open_interest: None,
            premium: None,
        }
    }

    /// The schema this join produces factor rows against (== `FeatureSchema::from_catalogue(cfg)`).
    #[must_use]
    pub fn schema(&self) -> FeatureSchema {
        self.assembler.schema()
    }

    /// Record the latest funding rate (the as-of snapshot updates; applies to subsequent bars).
    pub fn observe_funding(&mut self, value: Decimal) {
        self.funding = Some(value);
    }

    /// Record the latest open interest.
    pub fn observe_open_interest(&mut self, value: Decimal) {
        self.open_interest = Some(value);
    }

    /// Record the latest premium (perp − underlier).
    pub fn observe_premium(&mut self, value: Decimal) {
        self.premium = Some(value);
    }

    /// Join the current context snapshot onto `bar` and assemble its factor row.
    pub fn on_bar(&mut self, bar: &Bar) -> FeatureVector {
        let sample = Sample {
            bar: bar.clone(),
            funding: self.funding,
            open_interest: self.open_interest,
            premium: self.premium,
        };
        self.assembler.push(&sample)
    }

    /// Reset the catalogue to its pre-warm state and clear the context snapshot.
    pub fn reset(&mut self) {
        self.assembler.reset();
        self.funding = None;
        self.open_interest = None;
        self.premium = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qe_domain::{Price, Qty, Resolution, Timestamp};
    use qe_signal::assemble_batch;
    use rust_decimal::Decimal;

    const MIN: i64 = 60_000;

    fn dec(n: i64) -> Decimal {
        Decimal::from(n)
    }
    fn p(n: i64) -> Price {
        Price::new(dec(n)).unwrap()
    }
    fn q(n: i64) -> Qty {
        Qty::new(dec(n)).unwrap()
    }

    /// A 5m base bar at the `slot`-th 5-minute slot, with a little price variation.
    fn base_bar(slot: i64) -> Bar {
        let base = 100 + (slot % 11);
        Bar::new(
            Timestamp::from_millis(slot * 5 * MIN),
            Resolution::M5,
            p(base),
            p(base + 5),
            p(base - 4),
            p(base + 1),
            q(10 + (slot % 7)),
            1 + (slot as u64 % 3),
        )
        .unwrap()
    }

    /// A timestamped scalar context observation.
    #[derive(Clone, Copy)]
    enum Obs {
        Funding(i64, i64),
        OpenInterest(i64, i64),
        Premium(i64, i64),
    }
    impl Obs {
        fn ts(self) -> i64 {
            match self {
                Obs::Funding(t, _) | Obs::OpenInterest(t, _) | Obs::Premium(t, _) => t,
            }
        }
    }

    /// Offline batch as-of join: for each bar, snapshot the latest context value with `ts <= bar_time`
    /// (context with equal ts counts). Independent of the streaming snapshot — the parity reference.
    fn as_of_join(bars: &[Bar], obs: &[Obs]) -> Vec<Sample> {
        bars.iter()
            .map(|bar| {
                let t = bar.open_time().millis();
                let mut funding = None;
                let mut open_interest = None;
                let mut premium = None;
                for o in obs.iter().filter(|o| o.ts() <= t) {
                    match *o {
                        Obs::Funding(_, v) => funding = Some(dec(v)),
                        Obs::OpenInterest(_, v) => open_interest = Some(dec(v)),
                        Obs::Premium(_, v) => premium = Some(dec(v)),
                    }
                }
                Sample {
                    bar: bar.clone(),
                    funding,
                    open_interest,
                    premium,
                }
            })
            .collect()
    }

    /// Drive the live join: interleave bars and context observations in timestamp order. Within an equal
    /// timestamp, context is applied before the bar (so `ts == bar_time` context is in force) — matching
    /// the offline `<=` rule.
    fn run_live(cfg: &CatalogueConfig, bars: &[Bar], obs: &[Obs]) -> Vec<FeatureVector> {
        let mut join = LiveFactorJoin::new(cfg);
        let mut oi = 0usize;
        let mut out = Vec::new();
        for bar in bars {
            let t = bar.open_time().millis();
            while oi < obs.len() && obs[oi].ts() <= t {
                match obs[oi] {
                    Obs::Funding(_, v) => join.observe_funding(dec(v)),
                    Obs::OpenInterest(_, v) => join.observe_open_interest(dec(v)),
                    Obs::Premium(_, v) => join.observe_premium(dec(v)),
                }
                oi += 1;
            }
            out.push(join.on_bar(bar));
        }
        out
    }

    fn cfg() -> CatalogueConfig {
        CatalogueConfig::default()
    }

    #[test]
    fn live_factor_rows_equal_offline_feature_vectors() {
        // 40 base bars + context observations at assorted times: before the first bar, between bars, and
        // exactly on a bar boundary.
        let bars: Vec<Bar> = (0..40).map(base_bar).collect();
        let obs = vec![
            Obs::Funding(-1, 7),                        // before the first bar
            Obs::OpenInterest(3 * 5 * MIN + 100, 5000), // between bar 3 and 4
            Obs::Premium(10 * 5 * MIN, 2),              // exactly on bar 10
            Obs::Funding(20 * 5 * MIN + 1, 9),          // just after bar 20
            Obs::OpenInterest(30 * 5 * MIN, 6000),
        ];

        let offline = assemble_batch(&cfg(), &as_of_join(&bars, &obs));
        let live = run_live(&cfg(), &bars, &obs);

        assert_eq!(
            live, offline,
            "live factor rows must equal offline feature vectors bar-for-bar"
        );
        // Non-trivial: at least one complete row, and the schema is the full catalogue (≥ 20 factors).
        assert!(live.iter().any(FeatureVector::is_complete));
        assert!(live[0].states.len() >= 20);
    }

    #[test]
    fn bar_only_parity_with_no_context() {
        let bars: Vec<Bar> = (0..40).map(base_bar).collect();
        let offline = assemble_batch(&cfg(), &as_of_join(&bars, &[]));
        let live = run_live(&cfg(), &bars, &[]);
        assert_eq!(live, offline);
        // Same as the canonical bar-only path.
        let samples: Vec<Sample> = bars.iter().cloned().map(Sample::from_bar).collect();
        assert_eq!(live, assemble_batch(&cfg(), &samples));
    }

    #[test]
    fn as_of_context_applies_to_the_right_bar() {
        // A funding observation arriving strictly between bar 0 and bar 1 must apply to bar 1, not bar 0
        // — proven via the offline as-of join (the canonical rule) plus the live run agreeing with it.
        let bars: Vec<Bar> = (0..3).map(base_bar).collect();
        let obs = vec![Obs::Funding(5 * MIN, 7)]; // ts between bar 0 (0) and bar 1 (5m)
        let samples = as_of_join(&bars, &obs);
        assert_eq!(samples[0].funding, None, "bar 0 precedes the observation");
        assert_eq!(
            samples[1].funding,
            Some(dec(7)),
            "bar 1 sees the observation"
        );
        assert_eq!(samples[2].funding, Some(dec(7)));
        // The live join agrees bar-for-bar.
        assert_eq!(
            run_live(&cfg(), &bars, &obs),
            assemble_batch(&cfg(), &samples)
        );
    }

    #[test]
    fn schema_equals_catalogue_schema() {
        let join = LiveFactorJoin::new(&cfg());
        assert_eq!(join.schema(), FeatureSchema::from_catalogue(&cfg()));
    }

    #[test]
    fn reset_reproduces_the_same_rows() {
        let bars: Vec<Bar> = (0..30).map(base_bar).collect();
        let obs = vec![Obs::Funding(0, 3), Obs::Premium(5 * 5 * MIN, 1)];
        let mut join = LiveFactorJoin::new(&cfg());
        let first = drive(&mut join, &bars, &obs);
        join.reset();
        let second = drive(&mut join, &bars, &obs);
        assert_eq!(
            first, second,
            "a reset join reproduces identical factor rows"
        );
    }

    fn drive(join: &mut LiveFactorJoin, bars: &[Bar], obs: &[Obs]) -> Vec<FeatureVector> {
        let mut oi = 0usize;
        let mut out = Vec::new();
        for bar in bars {
            let t = bar.open_time().millis();
            while oi < obs.len() && obs[oi].ts() <= t {
                match obs[oi] {
                    Obs::Funding(_, v) => join.observe_funding(dec(v)),
                    Obs::OpenInterest(_, v) => join.observe_open_interest(dec(v)),
                    Obs::Premium(_, v) => join.observe_premium(dec(v)),
                }
                oi += 1;
            }
            out.push(join.on_bar(bar));
        }
        out
    }
}
