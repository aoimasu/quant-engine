//! Dependency-free UTC calendar arithmetic (Howard Hinnant's `chrono`-algorithms). Deterministic and
//! wall-clock-free — the job never links a date crate, so `result.json` stays byte-reproducible.

/// Milliseconds per day.
const MS_PER_DAY: i64 = 86_400_000;

/// Days since the Unix epoch (1970-01-01) for a proleptic-Gregorian `y-m-d`.
/// `m ∈ 1..=12`, `d ∈ 1..=31`. (Howard Hinnant, `days_from_civil`.)
fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = i64::from(y) - i64::from(m <= 2);
    let m = i64::from(m);
    let d = i64::from(d);
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400; // [0, 399]
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719_468
}

/// Inverse of [`days_from_civil`]: `(year, month, day)` for a day-count since the epoch.
/// (Howard Hinnant, `civil_from_days`.)
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = y + i64::from(m <= 2);
    (y as i32, m as u32, d as u32)
}

/// Parse a strict `YYYY-MM-DD` string into epoch-milliseconds at UTC midnight.
///
/// Returns `None` on a malformed string or an out-of-range component (validated by round-tripping
/// through the calendar so e.g. `2021-02-30` is rejected).
#[must_use]
pub fn parse_ymd_to_millis(s: &str) -> Option<i64> {
    let mut it = s.split('-');
    let y: i32 = it.next()?.parse().ok()?;
    let mo: u32 = it.next()?.parse().ok()?;
    let d: u32 = it.next()?.parse().ok()?;
    if it.next().is_some() || !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    let days = days_from_civil(y, mo, d);
    // Reject invalid days (e.g. Feb 30) by round-tripping through the calendar.
    if civil_from_days(days) != (y, mo, d) {
        return None;
    }
    Some(days * MS_PER_DAY)
}

/// The `(year, month)` (`month ∈ 1..=12`) of an epoch-millisecond instant, UTC.
#[must_use]
pub fn year_month(millis: i64) -> (i32, u32) {
    let days = millis.div_euclid(MS_PER_DAY);
    let (y, m, _) = civil_from_days(days);
    (y, m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_is_zero() {
        assert_eq!(parse_ymd_to_millis("1970-01-01"), Some(0));
    }

    #[test]
    fn known_dates() {
        // 2021-01-01 = 18628 days since epoch.
        assert_eq!(parse_ymd_to_millis("2021-01-01"), Some(18628 * MS_PER_DAY));
        assert_eq!(year_month(18628 * MS_PER_DAY), (2021, 1));
    }

    #[test]
    fn rejects_malformed_and_out_of_range() {
        assert_eq!(parse_ymd_to_millis("2021-13-01"), None);
        assert_eq!(parse_ymd_to_millis("2021-02-30"), None);
        assert_eq!(parse_ymd_to_millis("2021-1-1x"), None);
        assert_eq!(parse_ymd_to_millis("2021/01/01"), None);
        assert_eq!(parse_ymd_to_millis("2021-01"), None);
        assert_eq!(parse_ymd_to_millis("2021-01-01-01"), None);
    }

    #[test]
    fn round_trips_across_a_range() {
        for &(y, m, d) in &[(1999, 12, 31), (2000, 2, 29), (2024, 2, 29), (2100, 3, 1)] {
            let ms = parse_ymd_to_millis(&format!("{y:04}-{m:02}-{d:02}")).unwrap();
            assert_eq!(year_month(ms), (y, m));
        }
    }
}
