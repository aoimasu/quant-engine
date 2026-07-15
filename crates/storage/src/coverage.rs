//! Read-only market-data coverage query (relocated from `qe-cli` for QE-257).
//!
//! [`coverage`] scans a [`MarketStore`] for the stored range + bar count of each `(instrument,
//! resolution)` pair. It lives here — a leaf/shared crate depending only on [`MarketStore`] +
//! `qe-domain` — so **both** `qe-cli` (the QE-253 `ingest` job re-exports it) and `qe-server` (the
//! QE-257 Market-data view) can call it without either taking a `qe-runtime`/`qe-venue` edge that the
//! QE-132/QE-254 firewall forbids.
//!
//! Deterministic: order and contents depend only on the store and the `instruments` slice — no
//! wall-clock, no RNG.

use qe_domain::{InstrumentId, Resolution};

use crate::{MarketStore, StorageError};

/// One row of the read-only market-data coverage query: the stored range + bar count for an
/// (instrument, resolution) pair. `from`/`to` are the **earliest / latest bar `open_time`** in
/// epoch-milliseconds (inclusive; `to` is the last bar's open time, not `open_time + resolution`).
///
/// A `std`/`serde`-only struct so the server crate (QE-257) can reuse the exact shape the CLI produces.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CoverageRow {
    /// Instrument symbol (e.g. `BTCUSDT`).
    pub symbol: String,
    /// Canonical resolution short code (e.g. `1h`).
    pub resolution: String,
    /// Earliest stored bar `open_time`, epoch-ms (inclusive).
    pub from: i64,
    /// Latest stored bar `open_time`, epoch-ms (inclusive).
    pub to: i64,
    /// Number of stored bars in `[from, to]`.
    pub bars: usize,
}

/// Scan `store` for every `(instrument, resolution)` pair and report the covered range + bar count.
///
/// For each instrument (in caller order) and each [`Resolution::ALL`] (ascending) that has at least one
/// bar, emits one [`CoverageRow`]. Deterministic: order and contents depend only on the store and the
/// `instruments` slice.
///
/// # Errors
/// [`StorageError`] on an LMDB failure while scanning.
pub fn coverage(
    store: &MarketStore,
    instruments: &[InstrumentId],
) -> Result<Vec<CoverageRow>, StorageError> {
    let mut rows = Vec::new();
    for instrument in instruments {
        for resolution in Resolution::ALL {
            // Key-only cursor over the (instrument, resolution) prefix: earliest/latest open_time and
            // the bar count come from the KEYS alone — no `Bar` value is decoded (QE-412).
            let Some((first, last, bars)) = store.coverage_bounds(instrument, resolution)? else {
                continue;
            };
            rows.push(CoverageRow {
                symbol: instrument.as_str().to_owned(),
                resolution: resolution.as_str().to_owned(),
                from: first.millis(),
                to: last.millis(),
                bars,
            });
        }
    }
    Ok(rows)
}

/// Coverage over **every instrument that has stored bars** — the enumeration the admin server's
/// Market-data view (QE-257) needs when it has no explicit universe to pass.
///
/// Enumerates instruments via [`MarketStore::bar_instruments`] (ascending, deterministic) then defers
/// to [`coverage`].
///
/// # Errors
/// [`StorageError`] on an LMDB failure while enumerating or scanning.
pub fn coverage_all(store: &MarketStore) -> Result<Vec<CoverageRow>, StorageError> {
    let instruments = store.bar_instruments()?;
    coverage(store, &instruments)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coverage_row_serialises_expected_shape() {
        let row = CoverageRow {
            symbol: "BTCUSDT".to_owned(),
            resolution: "1h".to_owned(),
            from: 1_609_459_200_000,
            to: 1_609_887_600_000,
            bars: 120,
        };
        let json = serde_json::to_string(&row).unwrap();
        assert_eq!(
            json,
            r#"{"symbol":"BTCUSDT","resolution":"1h","from":1609459200000,"to":1609887600000,"bars":120}"#
        );
        let back: CoverageRow = serde_json::from_str(&json).unwrap();
        assert_eq!(back, row);
    }
}
