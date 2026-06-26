//! Point-in-time instrument universe.
//!
//! A [`Universe`] is a set of [`InstrumentListing`]s, each a half-open tradability window
//! `[listed, delisted)`. [`Universe::members_at`] answers *which instruments were tradable at a
//! given instant* — so a backtest as-of some date never sees an instrument that was not yet listed
//! or had already been delisted (no survivorship bias). Delisted symbols are retained in
//! [`Universe::all_known`], never silently dropped.
//!
//! The universe is built from config (see [`crate::Config::universe`]); this module owns the value
//! types, the point-in-time query, and the ISO-date → [`Timestamp`] conversion.

use qe_domain::{InstrumentId, Timestamp};

/// The "listed since forever" sentinel for an open-ended listing (e.g. the backward-compatible
/// fallback from a flat `instruments` list, or a `[[universe]]` entry with no `listed` date).
/// `as_of >= MIN` always holds, so such an instrument is a member at every instant up to its
/// (absent) delisting.
pub const OPEN_LISTING: Timestamp = Timestamp::from_millis(i64::MIN);

/// One instrument's point-in-time tradability window `[listed, delisted)`.
///
/// `delisted = None` means *still trading* (no upper bound). The window is **half-open**, matching
/// [`qe_domain::TimeInterval`]: an instrument is tradable at `t` iff `listed <= t < delisted`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstrumentListing {
    instrument: InstrumentId,
    listed: Timestamp,
    delisted: Option<Timestamp>,
}

impl InstrumentListing {
    /// Construct a listing with an explicit window.
    ///
    /// # Errors
    /// Returns a message if `delisted < listed` (a window that closes before it opens).
    pub fn new(
        instrument: InstrumentId,
        listed: Timestamp,
        delisted: Option<Timestamp>,
    ) -> Result<Self, &'static str> {
        if let Some(d) = delisted {
            if d < listed {
                return Err("delisted date must not be before listed date");
            }
        }
        Ok(Self {
            instrument,
            listed,
            delisted,
        })
    }

    /// An open-ended listing (listed since forever, never delisted) — the fallback for a flat
    /// `instruments` entry with no dates: always a member.
    #[must_use]
    pub fn open_ended(instrument: InstrumentId) -> Self {
        Self {
            instrument,
            listed: OPEN_LISTING,
            delisted: None,
        }
    }

    /// The instrument.
    #[must_use]
    pub fn instrument(&self) -> &InstrumentId {
        &self.instrument
    }

    /// The listing instant (inclusive).
    #[must_use]
    pub fn listed(&self) -> Timestamp {
        self.listed
    }

    /// The delisting instant (exclusive), or `None` if still trading.
    #[must_use]
    pub fn delisted(&self) -> Option<Timestamp> {
        self.delisted
    }

    /// Whether this instrument was tradable at `as_of`: `listed <= as_of < delisted`.
    #[must_use]
    pub fn is_tradable_at(&self, as_of: Timestamp) -> bool {
        self.listed <= as_of && self.delisted.is_none_or(|d| as_of < d)
    }
}

/// A point-in-time instrument universe: the full roster of listings, queryable as-of any instant.
///
/// Built from validated config, so it contains no duplicate instruments. Count-agnostic — one
/// instrument or many flows through the identical code.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Universe {
    listings: Vec<InstrumentListing>,
}

impl Universe {
    /// Build a universe from listings (config order preserved).
    #[must_use]
    pub fn new(listings: Vec<InstrumentListing>) -> Self {
        Self { listings }
    }

    /// The instruments tradable at `as_of` — i.e. each listing whose half-open window contains it.
    /// An instrument not yet listed (`as_of < listed`) or already delisted (`as_of >= delisted`) is
    /// excluded. Returned in stable config order.
    #[must_use]
    pub fn members_at(&self, as_of: Timestamp) -> Vec<InstrumentId> {
        self.listings
            .iter()
            .filter(|l| l.is_tradable_at(as_of))
            .map(|l| l.instrument.clone())
            .collect()
    }

    /// Whether `instrument` was a tradable member at `as_of`.
    #[must_use]
    pub fn is_member_at(&self, instrument: &InstrumentId, as_of: Timestamp) -> bool {
        self.listings
            .iter()
            .any(|l| l.instrument == *instrument && l.is_tradable_at(as_of))
    }

    /// The **full** roster, including already-delisted instruments — so corpus loading never
    /// silently drops a blown-up symbol. Filtering to a point in time is an explicit
    /// [`members_at`](Self::members_at) call.
    #[must_use]
    pub fn all_known(&self) -> &[InstrumentListing] {
        &self.listings
    }

    /// Number of instruments in the universe (tradable or not).
    #[must_use]
    pub fn len(&self) -> usize {
        self.listings.len()
    }

    /// Whether the universe is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.listings.is_empty()
    }
}

/// Parse an ISO `YYYY-MM-DD` date to a [`Timestamp`] at **UTC midnight** (`00:00:00Z`).
///
/// Calendar-validates the month and per-month day (including leap years), so `2020-13-01` and
/// `2021-02-29` are rejected. No external date crate — a pure civil-date algorithm keeps the
/// conversion deterministic across machines.
///
/// # Errors
/// Returns a message if the string is not a well-formed, in-range ISO date.
pub fn parse_iso_date(s: &str) -> Result<Timestamp, &'static str> {
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return Err("date must be ISO `YYYY-MM-DD`");
    }
    let digits = |slice: &str| slice.bytes().all(|c| c.is_ascii_digit());
    if !(digits(&s[0..4]) && digits(&s[5..7]) && digits(&s[8..10])) {
        return Err("date must be ISO `YYYY-MM-DD`");
    }
    let year: i64 = s[0..4].parse().map_err(|_| "invalid year")?;
    let month: u32 = s[5..7].parse().map_err(|_| "invalid month")?;
    let day: u32 = s[8..10].parse().map_err(|_| "invalid day")?;
    if !(1..=12).contains(&month) {
        return Err("month out of range 1..=12");
    }
    if day < 1 || day > days_in_month(year, month) {
        return Err("day out of range for month");
    }
    let days = days_from_civil(year, month, day);
    Ok(Timestamp::from_millis(days * 86_400_000))
}

/// Days in `month` of `year`, honouring leap years.
fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Proleptic Gregorian leap-year rule.
fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// Days since the Unix epoch for a proleptic-Gregorian civil date (Howard Hinnant's algorithm).
/// Exact and branch-light; valid across the full `i64` year range we care about.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let m = i64::from(m);
    let d = i64::from(d);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inst(s: &str) -> InstrumentId {
        InstrumentId::new(s).unwrap()
    }
    fn date(s: &str) -> Timestamp {
        parse_iso_date(s).unwrap()
    }

    #[test]
    fn days_from_civil_golden_values() {
        // Epoch and a couple of known anchors.
        assert_eq!(date("1970-01-01").millis(), 0);
        assert_eq!(date("1970-01-02").millis(), 86_400_000);
        // 2000-01-01 is 10_957 days after the epoch.
        assert_eq!(date("2000-01-01").millis(), 10_957 * 86_400_000);
        // A pre-epoch date is negative.
        assert_eq!(date("1969-12-31").millis(), -86_400_000);
    }

    #[test]
    fn parse_iso_date_rejects_malformed_and_out_of_range() {
        assert!(parse_iso_date("2020-13-01").is_err()); // month
        assert!(parse_iso_date("2021-02-29").is_err()); // not a leap year
        assert!(super::parse_iso_date("2020-02-29").is_ok()); // leap year
        assert!(parse_iso_date("2020-04-31").is_err()); // April has 30
        assert!(parse_iso_date("2020-1-1").is_err()); // width
        assert!(parse_iso_date("2020/01/01").is_err()); // separators
        assert!(parse_iso_date("banana").is_err());
    }

    #[test]
    fn members_at_respects_listing_and_delisting() {
        let u = Universe::new(vec![
            InstrumentListing::new(inst("BTCUSDT"), date("2019-09-08"), None).unwrap(),
            InstrumentListing::new(
                inst("ETHUSDT"),
                date("2019-11-27"),
                Some(date("2025-01-01")),
            )
            .unwrap(),
        ]);

        // Nothing listed yet.
        assert!(u.members_at(date("2019-01-01")).is_empty());
        // BTC listed, ETH not yet.
        assert_eq!(u.members_at(date("2019-10-01")), vec![inst("BTCUSDT")]);
        // Both live.
        assert_eq!(
            u.members_at(date("2020-01-01")),
            vec![inst("BTCUSDT"), inst("ETHUSDT")]
        );
        // ETH delisted.
        assert_eq!(u.members_at(date("2025-06-01")), vec![inst("BTCUSDT")]);
    }

    #[test]
    fn membership_boundaries_are_half_open() {
        let u = Universe::new(vec![InstrumentListing::new(
            inst("ETHUSDT"),
            date("2019-11-27"),
            Some(date("2025-01-01")),
        )
        .unwrap()]);
        // listed is inclusive; delisted is exclusive.
        assert!(u.is_member_at(&inst("ETHUSDT"), date("2019-11-27")));
        assert!(!u.is_member_at(&inst("ETHUSDT"), date("2025-01-01")));
        assert!(u.is_member_at(&inst("ETHUSDT"), date("2024-12-31")));
    }

    #[test]
    fn open_ended_listing_is_always_a_member() {
        let u = Universe::new(vec![InstrumentListing::open_ended(inst("BTCUSDT"))]);
        assert!(u.is_member_at(&inst("BTCUSDT"), date("1999-01-01")));
        assert!(u.is_member_at(&inst("BTCUSDT"), date("2099-01-01")));
    }

    #[test]
    fn delisted_symbols_stay_in_all_known() {
        let u = Universe::new(vec![InstrumentListing::new(
            inst("LUNAUSDT"),
            date("2020-01-01"),
            Some(date("2022-05-13")),
        )
        .unwrap()]);
        // No longer a member after delisting...
        assert!(u.members_at(date("2023-01-01")).is_empty());
        // ...but never silently dropped from the roster.
        assert_eq!(u.all_known().len(), 1);
        assert_eq!(u.all_known()[0].instrument(), &inst("LUNAUSDT"));
    }

    #[test]
    fn listing_rejects_delisted_before_listed() {
        let err = InstrumentListing::new(
            inst("BTCUSDT"),
            date("2020-01-01"),
            Some(date("2019-01-01")),
        )
        .unwrap_err();
        assert!(err.contains("before listed"));
    }

    #[test]
    fn universe_is_count_agnostic() {
        let one = Universe::new(vec![InstrumentListing::open_ended(inst("BTCUSDT"))]);
        let many = Universe::new(
            ["BTCUSDT", "ETHUSDT", "SOLUSDT"]
                .into_iter()
                .map(|s| InstrumentListing::open_ended(inst(s)))
                .collect(),
        );
        assert_eq!(one.members_at(date("2021-01-01")).len(), 1);
        assert_eq!(many.members_at(date("2021-01-01")).len(), 3);
    }
}
