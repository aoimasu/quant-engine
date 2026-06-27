//! Funding / open-interest / premium flow factors (QE-107). Same finite-window contract as the
//! price indicators, but reading the aligned scalar context instead of the bar; a step whose scalar
//! is absent is skipped, so the window holds the last `lookback` *present* scalars.

use rust_decimal::Decimal;

use super::roll::Roll;
use super::{Indicator, Kernel, Quantiser, Sample};

/// A finite-window indicator over one scalar series (funding / OI / premium).
struct ScalarKernel {
    id: String,
    q: Quantiser,
    roll: Roll,
    /// Pick this kernel's scalar out of a sample (`None` ⇒ skip the step).
    select: fn(&Sample) -> Option<Decimal>,
    /// Compute the raw value from the window.
    value: fn(&Roll) -> Option<Decimal>,
}

impl Kernel for ScalarKernel {
    fn id(&self) -> String {
        self.id.clone()
    }
    fn lookback(&self) -> usize {
        self.roll.cap()
    }
    fn quantiser(&self) -> &Quantiser {
        &self.q
    }
    fn observe(&mut self, sample: &Sample) {
        if let Some(v) = (self.select)(sample) {
            self.roll.push(v);
        }
    }
    fn warm(&self) -> bool {
        self.roll.is_full()
    }
    fn raw(&self) -> Option<Decimal> {
        (self.value)(&self.roll)
    }
    fn clear(&mut self) {
        self.roll = Roll::new(self.roll.cap());
    }
}

fn scalar(
    id: &str,
    lookback: usize,
    q: Quantiser,
    select: fn(&Sample) -> Option<Decimal>,
    value: fn(&Roll) -> Option<Decimal>,
) -> Box<dyn Indicator> {
    Box::new(ScalarKernel {
        id: id.to_owned(),
        q,
        roll: Roll::new(lookback),
        select,
        value,
    })
}

const HUNDRED: Decimal = Decimal::from_parts(100, 0, 0, false, 0);

// ---- selectors -------------------------------------------------------------------------------

fn sel_funding(s: &Sample) -> Option<Decimal> {
    s.funding
}
fn sel_oi(s: &Sample) -> Option<Decimal> {
    s.open_interest
}
fn sel_premium(s: &Sample) -> Option<Decimal> {
    s.premium
}

// ---- value functions -------------------------------------------------------------------------

fn latest(r: &Roll) -> Option<Decimal> {
    r.last()
}
fn mean(r: &Roll) -> Option<Decimal> {
    r.mean()
}
fn roc(r: &Roll) -> Option<Decimal> {
    let first = r.first()?;
    if first.is_zero() {
        None
    } else {
        Some((r.last()? / first - Decimal::ONE) * HUNDRED)
    }
}

/// A funding/premium rate band: roughly `±1%` per interval, quantised into `states` buckets.
fn rate_quant(states: u16) -> Quantiser {
    Quantiser::Linear {
        min: Decimal::new(-1, 2), // -0.01
        max: Decimal::new(1, 2),  //  0.01
        states,
    }
}

/// Append every funding/OI/premium flow factor to `out`.
pub(super) fn extend_catalogue(out: &mut Vec<Box<dyn Indicator>>, states: u16) {
    out.push(scalar(
        "funding_state",
        1,
        rate_quant(states),
        sel_funding,
        latest,
    ));
    out.push(scalar(
        "funding_avg_8",
        8,
        rate_quant(states),
        sel_funding,
        mean,
    ));
    out.push(scalar(
        "oi_roc_10",
        11,
        Quantiser::Linear {
            min: Decimal::from(-25),
            max: Decimal::from(25),
            states,
        },
        sel_oi,
        roc,
    ));
    out.push(scalar(
        "premium_state",
        1,
        rate_quant(states),
        sel_premium,
        latest,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indicator::CatalogueConfig;
    use qe_domain::{Bar, Price, Qty, Resolution, Timestamp};

    const MIN: i64 = 60_000;

    fn sample(funding: Option<i64>, oi: Option<i64>, premium: Option<i64>, i: i64) -> Sample {
        let p = Price::new(Decimal::from(100)).unwrap();
        Sample {
            bar: Bar::new(
                Timestamp::from_millis(i * 5 * MIN),
                Resolution::M5,
                p,
                p,
                p,
                p,
                Qty::new(Decimal::ONE).unwrap(),
                1,
            )
            .unwrap(),
            funding: funding.map(|f| Decimal::new(f, 4)),
            open_interest: oi.map(Decimal::from),
            premium: premium.map(|p| Decimal::new(p, 4)),
        }
    }

    #[test]
    fn funding_state_quantises_the_latest_rate() {
        let mut k = scalar("funding_state", 1, rate_quant(5), sel_funding, latest);
        // funding = +0.005 (top half of [-0.01, 0.01]) → upper buckets.
        let out = k.update(&sample(Some(50), None, None, 0)); // 0.0050
        assert!(out.is_some());
        assert!(out.unwrap().index() >= 3);
    }

    #[test]
    fn flow_indicator_skips_steps_with_absent_scalar() {
        // funding_avg_8 needs 8 *present* funding samples; absent ones don't fill the window.
        let mut k = scalar("funding_avg_8", 8, rate_quant(5), sel_funding, mean);
        for i in 0..7 {
            assert!(k.update(&sample(Some(10), None, None, i)).is_none());
        }
        // A step with no funding does not advance warmup.
        assert!(k.update(&sample(None, None, None, 7)).is_none());
        // The 8th present sample warms it.
        assert!(k.update(&sample(Some(10), None, None, 8)).is_some());
    }

    #[test]
    fn flow_factors_are_in_the_catalogue() {
        let ids: Vec<String> = crate::indicator::catalogue(&CatalogueConfig::default())
            .iter()
            .map(|i| i.spec().id)
            .collect();
        for want in [
            "funding_state",
            "funding_avg_8",
            "oi_roc_10",
            "premium_state",
        ] {
            assert!(ids.contains(&want.to_owned()), "missing {want}");
        }
    }
}
