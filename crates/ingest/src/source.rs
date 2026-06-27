//! The `data.binance.vision` public-dump layout: which file holds a given instrument's data for a
//! given kind and period, and the URL / cache key / checksum sidecar for it.
//!
//! Only USD-margined futures (`futures/um`) are modelled — the linear-perp universe (QE-012).

use qe_domain::{InstrumentId, Resolution, Timestamp};

/// The default `data.binance.vision` base URL (no trailing slash).
pub const DEFAULT_BASE_URL: &str = "https://data.binance.vision";

/// A calendar date (UTC), used to name daily dump files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Date {
    /// Four-digit year.
    pub year: i32,
    /// Month `1..=12`.
    pub month: u32,
    /// Day `1..=31`.
    pub day: u32,
}

/// A calendar month (UTC), used to name monthly dump files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct YearMonth {
    /// Four-digit year.
    pub year: i32,
    /// Month `1..=12`.
    pub month: u32,
}

impl YearMonth {
    /// The month after this one (rolls over December → January).
    #[must_use]
    pub fn succ(self) -> Self {
        if self.month == 12 {
            YearMonth {
                year: self.year + 1,
                month: 1,
            }
        } else {
            YearMonth {
                year: self.year,
                month: self.month + 1,
            }
        }
    }

    /// ISO `YYYY-MM`.
    #[must_use]
    pub fn iso(self) -> String {
        format!("{:04}-{:02}", self.year, self.month)
    }
}

impl Date {
    /// ISO `YYYY-MM-DD`.
    #[must_use]
    pub fn iso(self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

/// The aggregation period of a dump file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Period {
    /// A single UTC day.
    Daily(Date),
    /// A single UTC month.
    Monthly(YearMonth),
}

impl Period {
    /// `daily` or `monthly` — the path segment Binance uses.
    fn segment(self) -> &'static str {
        match self {
            Period::Daily(_) => "daily",
            Period::Monthly(_) => "monthly",
        }
    }

    /// The period's identifier as it appears in the file name (`YYYY-MM-DD` or `YYYY-MM`).
    fn label(self) -> String {
        match self {
            Period::Daily(d) => d.iso(),
            Period::Monthly(m) => m.iso(),
        }
    }

    /// The instant the period starts (UTC midnight of its first day) — reuses the tested config
    /// date parser so this crate adds no civil-date math. `None` only on an impossible bad date.
    #[must_use]
    pub fn start(self) -> Option<Timestamp> {
        let iso = match self {
            Period::Daily(d) => d.iso(),
            Period::Monthly(m) => format!("{:04}-{:02}-01", m.year, m.month),
        };
        qe_config::universe::parse_iso_date(&iso).ok()
    }
}

/// What kind of market data a dump file holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataKind {
    /// OHLCV klines at a resolution.
    Klines(Resolution),
    /// Premium-index klines at a resolution.
    PremiumIndexKlines(Resolution),
    /// Funding-rate history (monthly only on Binance).
    FundingRate,
    /// `/futures/data` metrics (open interest, long/short ratios, …) — daily only.
    Metrics,
}

impl DataKind {
    /// The directory segment under `…/um/<period>/` for this kind.
    fn dir(self) -> &'static str {
        match self {
            DataKind::Klines(_) => "klines",
            DataKind::PremiumIndexKlines(_) => "premiumIndexKlines",
            DataKind::FundingRate => "fundingRate",
            DataKind::Metrics => "metrics",
        }
    }

    /// The token used in the file name (`klines`/`premiumIndexKlines`/`fundingRate`/`metrics`).
    fn file_token(self) -> &'static str {
        self.dir()
    }

    /// The resolution sub-segment, if this kind is keyed by resolution.
    fn resolution(self) -> Option<Resolution> {
        match self {
            DataKind::Klines(r) | DataKind::PremiumIndexKlines(r) => Some(r),
            _ => None,
        }
    }
}

/// One downloadable dump file: an instrument's `kind` data for a `period`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DumpFile {
    /// The instrument.
    pub instrument: InstrumentId,
    /// What the file holds.
    pub kind: DataKind,
    /// The period it covers.
    pub period: Period,
}

impl DumpFile {
    /// Construct a dump-file descriptor.
    #[must_use]
    pub fn new(instrument: InstrumentId, kind: DataKind, period: Period) -> Self {
        Self {
            instrument,
            kind,
            period,
        }
    }

    /// The relative path under the base URL — also the local cache key. Shape:
    /// `data/futures/um/<period>/<dir>/<SYMBOL>[/<interval>]/<SYMBOL>-<token>[-<interval>]-<label>.zip`.
    #[must_use]
    pub fn relative_path(&self) -> String {
        let sym = self.instrument.as_str();
        let period = self.period.segment();
        let dir = self.kind.dir();
        let token = self.kind.file_token();
        let label = self.period.label();
        match self.kind.resolution() {
            Some(res) => {
                let iv = res.as_str();
                format!("data/futures/um/{period}/{dir}/{sym}/{iv}/{sym}-{iv}-{label}.zip")
            }
            None => {
                format!("data/futures/um/{period}/{dir}/{sym}/{sym}-{token}-{label}.zip")
            }
        }
    }

    /// The full download URL for `base` (no trailing slash, e.g. [`DEFAULT_BASE_URL`]).
    #[must_use]
    pub fn url(&self, base: &str) -> String {
        format!("{base}/{}", self.relative_path())
    }

    /// The relative path of the `.CHECKSUM` sidecar.
    #[must_use]
    pub fn checksum_relative_path(&self) -> String {
        format!("{}.CHECKSUM", self.relative_path())
    }

    /// The full URL of the `.CHECKSUM` sidecar.
    #[must_use]
    pub fn checksum_url(&self, base: &str) -> String {
        format!("{base}/{}", self.checksum_relative_path())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inst(s: &str) -> InstrumentId {
        InstrumentId::new(s).unwrap()
    }

    #[test]
    fn daily_kline_path_is_correct() {
        let f = DumpFile::new(
            inst("BTCUSDT"),
            DataKind::Klines(Resolution::M5),
            Period::Daily(Date {
                year: 2020,
                month: 1,
                day: 7,
            }),
        );
        assert_eq!(
            f.relative_path(),
            "data/futures/um/daily/klines/BTCUSDT/5m/BTCUSDT-5m-2020-01-07.zip"
        );
        assert_eq!(
            f.url(DEFAULT_BASE_URL),
            "https://data.binance.vision/data/futures/um/daily/klines/BTCUSDT/5m/BTCUSDT-5m-2020-01-07.zip"
        );
        assert_eq!(
            f.checksum_relative_path(),
            "data/futures/um/daily/klines/BTCUSDT/5m/BTCUSDT-5m-2020-01-07.zip.CHECKSUM"
        );
    }

    #[test]
    fn monthly_funding_path_has_no_interval() {
        let f = DumpFile::new(
            inst("ETHUSDT"),
            DataKind::FundingRate,
            Period::Monthly(YearMonth {
                year: 2021,
                month: 11,
            }),
        );
        assert_eq!(
            f.relative_path(),
            "data/futures/um/monthly/fundingRate/ETHUSDT/ETHUSDT-fundingRate-2021-11.zip"
        );
    }

    #[test]
    fn daily_metrics_path_is_correct() {
        let f = DumpFile::new(
            inst("BTCUSDT"),
            DataKind::Metrics,
            Period::Daily(Date {
                year: 2022,
                month: 12,
                day: 31,
            }),
        );
        assert_eq!(
            f.relative_path(),
            "data/futures/um/daily/metrics/BTCUSDT/BTCUSDT-metrics-2022-12-31.zip"
        );
    }

    #[test]
    fn period_start_and_month_succ() {
        assert_eq!(
            Period::Daily(Date {
                year: 1970,
                month: 1,
                day: 2
            })
            .start()
            .unwrap()
            .millis(),
            86_400_000
        );
        assert_eq!(
            YearMonth {
                year: 2020,
                month: 12
            }
            .succ(),
            YearMonth {
                year: 2021,
                month: 1
            }
        );
        assert_eq!(
            Period::Monthly(YearMonth {
                year: 2020,
                month: 2
            })
            .start()
            .unwrap()
            .millis(),
            Period::Daily(Date {
                year: 2020,
                month: 2,
                day: 1
            })
            .start()
            .unwrap()
            .millis()
        );
    }
}
