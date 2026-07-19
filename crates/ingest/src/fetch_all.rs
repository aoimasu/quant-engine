//! Fetch-all instrument resolution via the point-in-time universe machinery (QE-464).
//!
//! "Fetch all available instruments" must **not** be a silent open-ended universe — that reintroduces
//! survivorship bias (QE-448). Fetch-all resolves through the **existing** as-of universe machinery
//! ([`qe_config::Universe`] + its `[listed, delisted)` windows, and [`crate::plan::enumerate_targets`]
//! which intersects each window with the fetch range): the resolved set is the **full known roster**
//! (delisted names retained for max point-in-time history), and the as-of survivorship **kill** at
//! backtest time is [`qe_config::Universe::members_at`] (which excludes not-yet-listed / already-
//! delisted instruments).
//!
//! If the config carries **no** listing dates (a flat `instruments` list ⇒ every listing is
//! `open_ended`), fetch-all is flagged [`FetchAllResolution::survivorship_unsafe`] rather than treated
//! as a trustworthy point-in-time universe.

use qe_config::universe::OPEN_LISTING;
use qe_config::Universe;
use qe_domain::InstrumentId;

/// The resolved fetch-all instrument set plus the survivorship-safety verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchAllResolution {
    /// The full known roster to fetch — delisted names retained (max point-in-time history). Stable
    /// config order.
    pub instruments: Vec<InstrumentId>,
    /// `true` when the universe carries no listing dates (every entry is open-ended), so a fetch-all
    /// here cannot be trusted as survivorship-safe. The caller must flag the resulting store
    /// `survivorship-unsafe` rather than treat it as a point-in-time universe.
    pub survivorship_unsafe: bool,
}

/// Resolve "fetch all available instruments" against the point-in-time `universe`.
///
/// Returns the full known roster (incl. delisted) and whether the universe is survivorship-unsafe (no
/// listing dates). The as-of exclusion of not-yet-listed / already-delisted instruments is
/// [`Universe::members_at`], applied at backtest time.
#[must_use]
pub fn resolve_fetch_all(universe: &Universe) -> FetchAllResolution {
    let instruments: Vec<InstrumentId> = universe
        .all_known()
        .iter()
        .map(|l| l.instrument().clone())
        .collect();
    // Survivorship-unsafe iff there are no listings, or EVERY listing is open-ended (no `listed` date):
    // such a universe has no point-in-time structure to kill survivorship with.
    let survivorship_unsafe = universe.is_empty()
        || universe
            .all_known()
            .iter()
            .all(|l| l.listed() == OPEN_LISTING);
    FetchAllResolution {
        instruments,
        survivorship_unsafe,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qe_config::universe::parse_iso_date;
    use qe_config::InstrumentListing;

    fn inst(s: &str) -> InstrumentId {
        InstrumentId::new(s).unwrap()
    }
    fn date(s: &str) -> qe_domain::Timestamp {
        parse_iso_date(s).unwrap()
    }

    #[test]
    fn dated_universe_resolves_full_roster_and_is_survivorship_safe() {
        let u = Universe::new(vec![
            InstrumentListing::new(inst("BTCUSDT"), date("2019-09-08"), None).unwrap(),
            // A delisted name is retained in the fetch-all roster (max history), never silently dropped.
            InstrumentListing::new(
                inst("LUNAUSDT"),
                date("2020-01-01"),
                Some(date("2022-05-13")),
            )
            .unwrap(),
        ]);
        let res = resolve_fetch_all(&u);
        assert_eq!(res.instruments, vec![inst("BTCUSDT"), inst("LUNAUSDT")]);
        assert!(
            !res.survivorship_unsafe,
            "a dated universe is survivorship-safe"
        );
    }

    #[test]
    fn as_of_backtest_excludes_not_yet_listed_and_delisted() {
        // The survivorship KILL: members_at is the existing as-of machinery fetch-all routes through.
        let u = Universe::new(vec![
            InstrumentListing::new(inst("BTCUSDT"), date("2019-09-08"), None).unwrap(),
            InstrumentListing::new(
                inst("LUNAUSDT"),
                date("2020-01-01"),
                Some(date("2022-05-13")),
            )
            .unwrap(),
        ]);
        // Before LUNA listed: excluded (not-yet-listed).
        assert_eq!(u.members_at(date("2019-10-01")), vec![inst("BTCUSDT")]);
        // While both live: both present.
        assert_eq!(
            u.members_at(date("2021-01-01")),
            vec![inst("BTCUSDT"), inst("LUNAUSDT")]
        );
        // After LUNA delisted: excluded (already-delisted).
        assert_eq!(u.members_at(date("2023-01-01")), vec![inst("BTCUSDT")]);
    }

    #[test]
    fn flat_open_ended_universe_is_flagged_survivorship_unsafe() {
        // A flat `instruments` list falls back to open-ended listings (no dates) — fetch-all here is
        // flagged, never silently treated as an open-ended point-in-time universe.
        let u = Universe::new(vec![
            InstrumentListing::open_ended(inst("BTCUSDT")),
            InstrumentListing::open_ended(inst("ETHUSDT")),
        ]);
        let res = resolve_fetch_all(&u);
        assert_eq!(res.instruments, vec![inst("BTCUSDT"), inst("ETHUSDT")]);
        assert!(
            res.survivorship_unsafe,
            "no listing dates ⇒ survivorship-unsafe"
        );

        // An empty universe is likewise survivorship-unsafe (nothing to point-in-time with).
        assert!(resolve_fetch_all(&Universe::default()).survivorship_unsafe);
    }
}
