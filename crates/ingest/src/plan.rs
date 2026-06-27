//! Point-in-time enumeration of download targets.
//!
//! Given the configured [`Universe`](qe_config::Universe), the data kinds, and a `[from, to]` month
//! window, produce the [`DumpFile`]s to fetch — intersecting each instrument's `[listed, delisted)`
//! window so we never request data from before listing or after delisting (max-available
//! point-in-time history, survivorship-bias-free via QE-012).

use qe_config::Universe;

use crate::source::{DataKind, Date, DumpFile, Period, YearMonth};

/// File granularity for a kind on `data.binance.vision`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Granularity {
    /// One file per UTC day (e.g. `/futures/data` metrics).
    Daily,
    /// One file per UTC month (bulk long-range klines / funding / premium index).
    Monthly,
}

impl DataKind {
    fn granularity(self) -> Granularity {
        match self {
            // Metrics dumps are published per-day only.
            DataKind::Metrics => Granularity::Daily,
            // Klines, premium-index, funding all have monthly bulk dumps.
            DataKind::Klines(_) | DataKind::PremiumIndexKlines(_) | DataKind::FundingRate => {
                Granularity::Monthly
            }
        }
    }
}

/// Days in a month, leap-year aware (for daily enumeration).
fn month_days(ym: YearMonth) -> u32 {
    let leap = (ym.year % 4 == 0 && ym.year % 100 != 0) || ym.year % 400 == 0;
    match ym.month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => 0,
    }
}

/// Inclusive month iterator from `from` to `to` (ascending). Empty if `to < from`.
fn months(from: YearMonth, to: YearMonth) -> Vec<YearMonth> {
    let mut out = Vec::new();
    let mut cur = from;
    while (cur.year, cur.month) <= (to.year, to.month) {
        out.push(cur);
        cur = cur.succ();
    }
    out
}

/// Whether a period `[p_start, p_end)` overlaps an instrument's `[listed, delisted)` window.
fn overlaps(
    period: Period,
    p_end_millis: i64,
    listed_millis: i64,
    delisted_millis: Option<i64>,
) -> bool {
    let Some(p_start) = period.start() else {
        return false;
    };
    let after_listing = listed_millis < p_end_millis;
    let before_delisting = delisted_millis.is_none_or(|d| p_start.millis() < d);
    after_listing && before_delisting
}

/// Enumerate the dump files to fetch for `kinds` over the inclusive `[from, to]` month window,
/// intersected per-instrument with its point-in-time listing window.
#[must_use]
pub fn enumerate_targets(
    universe: &Universe,
    kinds: &[DataKind],
    from: YearMonth,
    to: YearMonth,
) -> Vec<DumpFile> {
    let mut targets = Vec::new();
    for listing in universe.all_known() {
        let listed = listing.listed().millis();
        let delisted = listing.delisted().map(|d| d.millis());
        for &kind in kinds {
            match kind.granularity() {
                Granularity::Monthly => {
                    for ym in months(from, to) {
                        let period = Period::Monthly(ym);
                        let p_end = Period::Monthly(ym.succ())
                            .start()
                            .map_or(i64::MAX, |t| t.millis());
                        if overlaps(period, p_end, listed, delisted) {
                            targets.push(DumpFile::new(listing.instrument().clone(), kind, period));
                        }
                    }
                }
                Granularity::Daily => {
                    for ym in months(from, to) {
                        for day in 1..=month_days(ym) {
                            let date = Date {
                                year: ym.year,
                                month: ym.month,
                                day,
                            };
                            let period = Period::Daily(date);
                            let p_end =
                                period.start().map_or(i64::MAX, |t| t.millis() + 86_400_000);
                            if overlaps(period, p_end, listed, delisted) {
                                targets.push(DumpFile::new(
                                    listing.instrument().clone(),
                                    kind,
                                    period,
                                ));
                            }
                        }
                    }
                }
            }
        }
    }
    targets
}

#[cfg(test)]
mod tests {
    use super::*;
    use qe_config::universe::parse_iso_date;
    use qe_config::InstrumentListing;
    use qe_domain::{InstrumentId, Resolution};

    fn inst(s: &str) -> InstrumentId {
        InstrumentId::new(s).unwrap()
    }
    fn ym(y: i32, m: u32) -> YearMonth {
        YearMonth { year: y, month: m }
    }

    #[test]
    fn month_iteration_and_days() {
        assert_eq!(months(ym(2020, 11), ym(2021, 2)).len(), 4);
        assert!(months(ym(2021, 1), ym(2020, 1)).is_empty());
        assert_eq!(month_days(ym(2020, 2)), 29); // leap
        assert_eq!(month_days(ym(2021, 2)), 28);
        assert_eq!(month_days(ym(2021, 4)), 30);
    }

    #[test]
    fn monthly_targets_respect_listing_window() {
        let u = Universe::new(vec![InstrumentListing::new(
            inst("ETHUSDT"),
            parse_iso_date("2020-03-15").unwrap(),
            Some(parse_iso_date("2020-06-01").unwrap()),
        )
        .unwrap()]);
        let files = enumerate_targets(&u, &[DataKind::FundingRate], ym(2020, 1), ym(2020, 8));
        // Listed mid-March, delisted June 1 → months that overlap [Mar 15, Jun 1): Mar, Apr, May.
        let labels: Vec<String> = files.iter().map(|f| f.relative_path()).collect();
        assert_eq!(files.len(), 3, "got {labels:?}");
        assert!(labels
            .iter()
            .all(|p| p.contains("ETHUSDT-fundingRate-2020-0")));
        assert!(labels.iter().any(|p| p.contains("2020-03")));
        assert!(labels.iter().any(|p| p.contains("2020-05")));
        assert!(!labels.iter().any(|p| p.contains("2020-02"))); // before listing
        assert!(!labels.iter().any(|p| p.contains("2020-06"))); // delisted
    }

    #[test]
    fn open_ended_instrument_gets_whole_window() {
        let u = Universe::new(vec![InstrumentListing::open_ended(inst("BTCUSDT"))]);
        let files = enumerate_targets(
            &u,
            &[DataKind::Klines(Resolution::M5)],
            ym(2020, 1),
            ym(2020, 3),
        );
        assert_eq!(files.len(), 3); // Jan, Feb, Mar
    }

    #[test]
    fn daily_metrics_expand_per_day_within_listing() {
        let u = Universe::new(vec![InstrumentListing::new(
            inst("BTCUSDT"),
            parse_iso_date("2020-02-10").unwrap(),
            None,
        )
        .unwrap()]);
        // Feb 2020 (leap, 29 days); listed Feb 10 → days 10..=29 = 20 files.
        let files = enumerate_targets(&u, &[DataKind::Metrics], ym(2020, 2), ym(2020, 2));
        assert_eq!(files.len(), 20);
        assert!(files[0]
            .relative_path()
            .contains("metrics/BTCUSDT/BTCUSDT-metrics-2020-02-10"));
    }

    #[test]
    fn count_agnostic_over_universe() {
        let u = Universe::new(
            ["BTCUSDT", "ETHUSDT", "SOLUSDT"]
                .into_iter()
                .map(|s| InstrumentListing::open_ended(inst(s)))
                .collect(),
        );
        let files = enumerate_targets(&u, &[DataKind::FundingRate], ym(2021, 1), ym(2021, 1));
        assert_eq!(files.len(), 3); // one month × three instruments
    }
}
