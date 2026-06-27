//! Price/volume indicators (QE-107). Every indicator is the same finite-window [`BarsKernel`]
//! parameterised by a value function over the rolling window, so the latest output depends on
//! exactly the last `lookback` bars (AC #2) and batch == streaming (AC #1).

use rust_decimal::{Decimal, MathematicalOps};

use super::{Bars, Indicator, Kernel, Quantiser, Sample};

/// A finite-window indicator over the bar fields, quantised by `q`. The window length **is** the
/// declared lookback.
struct BarsKernel {
    id: String,
    q: Quantiser,
    bars: Bars,
    value: fn(&Bars) -> Option<Decimal>,
}

impl Kernel for BarsKernel {
    fn id(&self) -> String {
        self.id.clone()
    }
    fn lookback(&self) -> usize {
        self.bars.close.cap()
    }
    fn quantiser(&self) -> &Quantiser {
        &self.q
    }
    fn observe(&mut self, sample: &Sample) {
        self.bars.observe(&sample.bar);
    }
    fn warm(&self) -> bool {
        self.bars.is_full()
    }
    fn raw(&self) -> Option<Decimal> {
        (self.value)(&self.bars)
    }
    fn clear(&mut self) {
        self.bars.clear();
    }
}

fn lin(min: i64, max: i64, states: u16) -> Quantiser {
    Quantiser::Linear {
        min: Decimal::from(min),
        max: Decimal::from(max),
        states,
    }
}

fn kernel(
    id: &str,
    lookback: usize,
    q: Quantiser,
    value: fn(&Bars) -> Option<Decimal>,
) -> Box<dyn Indicator> {
    Box::new(BarsKernel {
        id: id.to_owned(),
        q,
        bars: Bars::new(lookback),
        value,
    })
}

const HUNDRED: Decimal = Decimal::from_parts(100, 0, 0, false, 0);

fn pct_change(from: Decimal, to: Decimal) -> Option<Decimal> {
    if from.is_zero() {
        None
    } else {
        Some((to / from - Decimal::ONE) * HUNDRED)
    }
}

/// Seeded windowed EMA over `values` with the given `period` (FIR: depends only on the slice).
fn ema_window(values: &[Decimal], period: usize) -> Option<Decimal> {
    let first = *values.first()?;
    let k = Decimal::from(2) / Decimal::from(period as i64 + 1);
    let mut ema = first;
    for &v in &values[1..] {
        ema += k * (v - ema);
    }
    Some(ema)
}

// ---- value functions (one per indicator) ----------------------------------------------------

fn sma_ratio(b: &Bars) -> Option<Decimal> {
    pct_change(b.close.mean()?, b.close.last()?)
}

fn ema_ratio(b: &Bars) -> Option<Decimal> {
    let v: Vec<Decimal> = b.close.iter().collect();
    let ema = ema_window(&v, v.len())?;
    pct_change(ema, b.close.last()?)
}

fn rsi(b: &Bars) -> Option<Decimal> {
    let c: Vec<Decimal> = b.close.iter().collect();
    let (mut gain, mut loss) = (Decimal::ZERO, Decimal::ZERO);
    for w in c.windows(2) {
        let d = w[1] - w[0];
        if d.is_sign_positive() {
            gain += d;
        } else {
            loss += -d;
        }
    }
    if loss.is_zero() {
        return Some(HUNDRED);
    }
    let rs = gain / loss;
    Some(HUNDRED - HUNDRED / (Decimal::ONE + rs))
}

fn stoch_k(b: &Bars) -> Option<Decimal> {
    let (hi, lo, c) = (b.high.max()?, b.low.min()?, b.close.last()?);
    let range = hi - lo;
    if range.is_zero() {
        return Some(HUNDRED / Decimal::from(2));
    }
    Some((c - lo) / range * HUNDRED)
}

fn williams_r(b: &Bars) -> Option<Decimal> {
    let (hi, lo, c) = (b.high.max()?, b.low.min()?, b.close.last()?);
    let range = hi - lo;
    if range.is_zero() {
        return Some(-HUNDRED / Decimal::from(2));
    }
    Some((hi - c) / range * -HUNDRED)
}

fn roc(b: &Bars) -> Option<Decimal> {
    pct_change(b.close.first()?, b.close.last()?)
}

fn cci(b: &Bars) -> Option<Decimal> {
    let tp = b.typical.last()?;
    let sma = b.typical.mean()?;
    let mad = b.typical.mean_abs_dev()?;
    if mad.is_zero() {
        return Some(Decimal::ZERO);
    }
    Some((tp - sma) / (Decimal::new(15, 3) * mad)) // 0.015 * mean_abs_dev
}

fn bollinger_bounds(b: &Bars) -> Option<(Decimal, Decimal, Decimal, Decimal)> {
    let mean = b.close.mean()?;
    let sd = b.close.std_pop()?;
    let two_sd = Decimal::from(2) * sd;
    Some((mean, sd, mean - two_sd, mean + two_sd))
}

fn bb_percent(b: &Bars) -> Option<Decimal> {
    let (_mean, _sd, lower, upper) = bollinger_bounds(b)?;
    let width = upper - lower;
    if width.is_zero() {
        return Some(Decimal::new(5, 1)); // mid-band
    }
    Some((b.close.last()? - lower) / width)
}

fn bb_bandwidth(b: &Bars) -> Option<Decimal> {
    let (mean, _sd, lower, upper) = bollinger_bounds(b)?;
    if mean.is_zero() {
        return None;
    }
    Some((upper - lower) / mean * HUNDRED)
}

fn mfi(b: &Bars) -> Option<Decimal> {
    let tp: Vec<Decimal> = b.typical.iter().collect();
    let vol: Vec<Decimal> = b.volume.iter().collect();
    let (mut pos, mut neg) = (Decimal::ZERO, Decimal::ZERO);
    for i in 1..tp.len() {
        let flow = tp[i] * vol[i];
        if tp[i] > tp[i - 1] {
            pos += flow;
        } else if tp[i] < tp[i - 1] {
            neg += flow;
        }
    }
    if neg.is_zero() {
        return Some(HUNDRED);
    }
    let mfr = pos / neg;
    Some(HUNDRED - HUNDRED / (Decimal::ONE + mfr))
}

fn aroon_osc(b: &Bars) -> Option<Decimal> {
    let highs: Vec<Decimal> = b.high.iter().collect();
    let lows: Vec<Decimal> = b.low.iter().collect();
    let n = highs.len();
    if n < 2 {
        return None;
    }
    // Index of the most-recent max high / min low (newest wins ties).
    let mut hh = 0usize;
    let mut ll = 0usize;
    for i in 1..n {
        if highs[i] >= highs[hh] {
            hh = i;
        }
        if lows[i] <= lows[ll] {
            ll = i;
        }
    }
    let denom = Decimal::from(n as i64 - 1);
    let since_high = Decimal::from((n - 1 - hh) as i64);
    let since_low = Decimal::from((n - 1 - ll) as i64);
    let up = (denom - since_high) / denom * HUNDRED;
    let down = (denom - since_low) / denom * HUNDRED;
    Some(up - down)
}

fn atr_pct(b: &Bars) -> Option<Decimal> {
    let h: Vec<Decimal> = b.high.iter().collect();
    let l: Vec<Decimal> = b.low.iter().collect();
    let c: Vec<Decimal> = b.close.iter().collect();
    let mut sum = Decimal::ZERO;
    let count = h.len() - 1;
    for i in 1..h.len() {
        let tr = (h[i] - l[i])
            .max((h[i] - c[i - 1]).abs())
            .max((l[i] - c[i - 1]).abs());
        sum += tr;
    }
    let atr = sum / Decimal::from(count as i64);
    let last = *c.last()?;
    if last.is_zero() {
        return None;
    }
    Some(atr / last * HUNDRED)
}

fn std_returns(b: &Bars) -> Option<Decimal> {
    let c: Vec<Decimal> = b.close.iter().collect();
    let mut rets = Vec::with_capacity(c.len() - 1);
    for w in c.windows(2) {
        if w[0].is_zero() {
            return None;
        }
        rets.push(w[1] / w[0] - Decimal::ONE);
    }
    let n = Decimal::from(rets.len() as i64);
    let mean = rets.iter().copied().sum::<Decimal>() / n;
    let var = rets
        .iter()
        .map(|&r| (r - mean) * (r - mean))
        .sum::<Decimal>()
        / n;
    Some(var.sqrt()? * HUNDRED)
}

fn volume_ratio(b: &Bars) -> Option<Decimal> {
    let mean = b.volume.mean()?;
    if mean.is_zero() {
        return None;
    }
    Some(b.volume.last()? / mean)
}

fn signed_volume_ratio(b: &Bars) -> Option<Decimal> {
    let c: Vec<Decimal> = b.close.iter().collect();
    let v: Vec<Decimal> = b.volume.iter().collect();
    let (mut signed, mut total) = (Decimal::ZERO, Decimal::ZERO);
    for i in 1..c.len() {
        total += v[i];
        if c[i] > c[i - 1] {
            signed += v[i];
        } else if c[i] < c[i - 1] {
            signed -= v[i];
        }
    }
    if total.is_zero() {
        return None;
    }
    Some(signed / total)
}

fn cmf(b: &Bars) -> Option<Decimal> {
    let h: Vec<Decimal> = b.high.iter().collect();
    let l: Vec<Decimal> = b.low.iter().collect();
    let c: Vec<Decimal> = b.close.iter().collect();
    let v: Vec<Decimal> = b.volume.iter().collect();
    let (mut mfv, mut vol) = (Decimal::ZERO, Decimal::ZERO);
    for i in 0..h.len() {
        let range = h[i] - l[i];
        let mfm = if range.is_zero() {
            Decimal::ZERO
        } else {
            ((c[i] - l[i]) - (h[i] - c[i])) / range
        };
        mfv += mfm * v[i];
        vol += v[i];
    }
    if vol.is_zero() {
        return None;
    }
    Some(mfv / vol)
}

fn macd_hist(b: &Bars) -> Option<Decimal> {
    // Window = slow + signal - 1 = 26 + 9 - 1 = 34. Build the last 9 MACD values, each an
    // ema(12) - ema(26) ending at its position; signal = their mean; histogram = macd_last - signal.
    const FAST: usize = 12;
    const SLOW: usize = 26;
    const SIGNAL: usize = 9;
    let c: Vec<Decimal> = b.close.iter().collect();
    if c.len() < SLOW + SIGNAL - 1 {
        return None;
    }
    let mut macd_line = Vec::with_capacity(SIGNAL);
    for end in (SLOW - 1)..c.len() {
        let fast = ema_window(&c[end + 1 - FAST..=end], FAST)?;
        let slow = ema_window(&c[end + 1 - SLOW..=end], SLOW)?;
        macd_line.push(fast - slow);
    }
    let signal = macd_line.iter().copied().sum::<Decimal>() / Decimal::from(macd_line.len() as i64);
    let hist = *macd_line.last()? - signal;
    let last = *c.last()?;
    if last.is_zero() {
        return None;
    }
    Some(hist / last * HUNDRED)
}

/// Append every price/volume indicator (configured for `states` quantised buckets) to `out`.
pub(super) fn extend_catalogue(out: &mut Vec<Box<dyn Indicator>>, states: u16) {
    out.push(kernel("sma_ratio_20", 20, lin(-10, 10, states), sma_ratio));
    out.push(kernel("ema_ratio_20", 20, lin(-10, 10, states), ema_ratio));
    out.push(kernel("rsi_14", 15, lin(0, 100, states), rsi));
    out.push(kernel("stoch_k_14", 14, lin(0, 100, states), stoch_k));
    out.push(kernel(
        "williams_r_14",
        14,
        lin(-100, 0, states),
        williams_r,
    ));
    out.push(kernel("roc_10", 11, lin(-25, 25, states), roc));
    out.push(kernel("return_1", 2, lin(-5, 5, states), roc));
    out.push(kernel("cci_20", 20, lin(-200, 200, states), cci));
    out.push(kernel(
        "bb_percent_20",
        20,
        bb_percent_quant(states),
        bb_percent,
    ));
    out.push(kernel(
        "bb_bandwidth_20",
        20,
        lin(0, 20, states),
        bb_bandwidth,
    ));
    out.push(kernel("mfi_14", 15, lin(0, 100, states), mfi));
    out.push(kernel(
        "aroon_osc_25",
        25,
        lin(-100, 100, states),
        aroon_osc,
    ));
    out.push(kernel("atr_pct_14", 15, lin(0, 10, states), atr_pct));
    out.push(kernel(
        "std_returns_20",
        21,
        lin(0, 10, states),
        std_returns,
    ));
    out.push(kernel(
        "volume_ratio_20",
        20,
        lin(0, 4, states),
        volume_ratio,
    ));
    out.push(kernel(
        "signed_volume_ratio_14",
        15,
        signed_unit_quant(states),
        signed_volume_ratio,
    ));
    out.push(kernel("cmf_20", 20, signed_unit_quant(states), cmf));
    out.push(kernel(
        "macd_hist_12_26_9",
        34,
        lin(-2, 2, states),
        macd_hist,
    ));
}

/// %B sits roughly in `[-0.5, 1.5]` — a fractional range, so build the linear quantiser directly.
fn bb_percent_quant(states: u16) -> Quantiser {
    Quantiser::Linear {
        min: Decimal::new(-5, 1),
        max: Decimal::new(15, 1),
        states,
    }
}

/// Unit signed range `[-1, 1]` (CMF, signed-volume ratio).
fn signed_unit_quant(states: u16) -> Quantiser {
    Quantiser::Linear {
        min: Decimal::from(-1),
        max: Decimal::from(1),
        states,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indicator::QState;
    use qe_domain::{Bar, Price, Qty, Resolution, Timestamp};

    const MIN: i64 = 60_000;

    fn d(n: i64) -> Decimal {
        Decimal::from(n)
    }

    fn bar(o: i64, h: i64, l: i64, c: i64, v: i64, i: i64) -> Sample {
        Sample::from_bar(
            Bar::new(
                Timestamp::from_millis(i * 5 * MIN),
                Resolution::M5,
                Price::new(d(o)).unwrap(),
                Price::new(d(h)).unwrap(),
                Price::new(d(l)).unwrap(),
                Price::new(d(c)).unwrap(),
                Qty::new(d(v)).unwrap(),
                1,
            )
            .unwrap(),
        )
    }

    fn run(value: fn(&Bars) -> Option<Decimal>, cap: usize, samples: &[Sample]) -> Option<Decimal> {
        let mut k = BarsKernel {
            id: "t".to_owned(),
            q: lin(0, 1, 2),
            bars: Bars::new(cap),
            value,
        };
        let mut last = None;
        for s in samples {
            k.observe(s);
            if k.warm() {
                last = k.raw();
            }
        }
        last
    }

    #[test]
    fn sma_ratio_matches_hand_computed() {
        // closes 10,20,30 → SMA 20; last 30 → (30/20 - 1)*100 = 50%.
        let s = vec![
            bar(10, 10, 10, 10, 1, 0),
            bar(20, 20, 20, 20, 1, 1),
            bar(30, 30, 30, 30, 1, 2),
        ];
        assert_eq!(run(sma_ratio, 3, &s), Some(d(50)));
    }

    #[test]
    fn rsi_all_gains_is_100() {
        let s: Vec<Sample> = (0..5)
            .map(|i| bar(10 + i, 10 + i, 10 + i, 10 + i, 1, i))
            .collect();
        // strictly rising closes → no losses → RSI 100.
        assert_eq!(run(rsi, 5, &s), Some(d(100)));
    }

    #[test]
    fn stoch_k_at_top_and_bottom() {
        // closes within [low=10, high=20]; last close 20 → %K = 100; last close 10 → 0.
        let top = vec![bar(15, 20, 10, 15, 1, 0), bar(15, 20, 10, 20, 1, 1)];
        assert_eq!(run(stoch_k, 2, &top), Some(d(100)));
        let bottom = vec![bar(15, 20, 10, 15, 1, 0), bar(15, 20, 10, 10, 1, 1)];
        assert_eq!(run(stoch_k, 2, &bottom), Some(d(0)));
    }

    #[test]
    fn roc_is_percent_over_window() {
        // first 100, last 110 → +10%.
        let s = vec![
            bar(100, 100, 100, 100, 1, 0),
            bar(0, 0, 0, 0, 1, 1),
            bar(110, 110, 110, 110, 1, 2),
        ];
        // window cap 3: first=100, last=110.
        assert_eq!(run(roc, 3, &s), Some(d(10)));
    }

    #[test]
    fn quantises_through_the_kernel() {
        // A 5-state RSI of a strictly-rising series (RSI 100) lands in the top bucket (4).
        let mut k = BarsKernel {
            id: "rsi".to_owned(),
            q: lin(0, 100, 5),
            bars: Bars::new(5),
            value: rsi,
        };
        let mut out: Option<QState> = None;
        for i in 0..5 {
            let s = bar(10 + i, 10 + i, 10 + i, 10 + i, 1, i);
            k.observe(&s);
            if k.warm() {
                out = k.raw().map(|v| k.quantiser().quantise(v));
            }
        }
        assert_eq!(out.unwrap().index(), 4);
    }
}
