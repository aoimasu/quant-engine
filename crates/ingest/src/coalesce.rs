//! Daily→monthly coalescence (QE-104): merge the per-partition bar vectors that the dumps/REST
//! paths produce into one ascending, duplicate-free series — the deterministic precondition for
//! temporal alignment.

use qe_domain::Bar;

/// Coalesce daily/monthly partitions into a single series sorted ascending by `open_time`.
///
/// On a duplicate `open_time` the **last** partition wins: the REST month-to-date backfill
/// (QE-102) is the fresher source and overrides an overlapping vendor-dump bar (QE-101),
/// consistent with the reconciliation stance in QE-103. The result is sorted and unique, so
/// alignment can assume a clean grid input.
#[must_use]
pub fn coalesce_bars(partitions: &[Vec<Bar>]) -> Vec<Bar> {
    // Stable index over a flattened view: later (partition, position) wins on a tie. Using a
    // BTreeMap keyed by open-time millis gives deterministic ascending output independent of the
    // input partition order's internal hashing.
    let mut by_time: std::collections::BTreeMap<i64, Bar> = std::collections::BTreeMap::new();
    for partition in partitions {
        for bar in partition {
            // insert overwrites → last-seen wins on duplicate open_time.
            by_time.insert(bar.open_time().millis(), bar.clone());
        }
    }
    by_time.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use qe_domain::{Price, Qty, Resolution, Timestamp};
    use rust_decimal::Decimal;

    fn bar_at(t_ms: i64, close: i64) -> Bar {
        let c = Price::new(Decimal::from(close)).unwrap();
        Bar::new(
            Timestamp::from_millis(t_ms),
            Resolution::M5,
            c,
            c,
            c,
            c,
            Qty::new(Decimal::ONE).unwrap(),
            1,
        )
        .unwrap()
    }

    #[test]
    fn merges_sorts_and_keeps_ascending() {
        // Two daily partitions, the second's timestamps interleave before/after the first.
        let day1 = vec![bar_at(0, 10), bar_at(300_000, 11)];
        let day2 = vec![bar_at(600_000, 12), bar_at(900_000, 13)];
        let out = coalesce_bars(&[day1, day2]);
        let times: Vec<i64> = out.iter().map(|b| b.open_time().millis()).collect();
        assert_eq!(times, vec![0, 300_000, 600_000, 900_000]);
    }

    #[test]
    fn duplicate_open_time_last_partition_wins() {
        // Same open_time in both partitions; the second (REST/fresher) close must win.
        let vendor = vec![bar_at(0, 10)];
        let rest = vec![bar_at(0, 99)];
        let out = coalesce_bars(&[vendor, rest]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].close().get(), Decimal::from(99));
    }

    #[test]
    fn already_sorted_unique_input_is_unchanged() {
        let bars = vec![bar_at(0, 1), bar_at(300_000, 2), bar_at(600_000, 3)];
        let out = coalesce_bars(std::slice::from_ref(&bars));
        assert_eq!(out, bars);
    }

    #[test]
    fn empty_input_is_empty() {
        assert!(coalesce_bars(&[]).is_empty());
        assert!(coalesce_bars(&[vec![]]).is_empty());
    }
}
