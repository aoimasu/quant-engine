//! Order-preserving byte-key encoding for LMDB range scans.
//!
//! LMDB compares keys lexicographically (default comparator), so keys are laid out as a fixed
//! prefix followed by a big-endian, sign-flipped timestamp — making byte order equal time order for
//! *all* `i64` (including negative). `InstrumentId` is validated ASCII-alphanumeric, so the `0x00`
//! delimiter never collides with a symbol byte and prefix boundaries are clean.

use qe_domain::{InstrumentId, Resolution, Timestamp};

const DELIM: u8 = 0x00;

/// Encode an `i64` so that unsigned big-endian byte comparison matches numeric order.
fn order_i64(v: i64) -> [u8; 8] {
    // Flip the sign bit: maps i64::MIN..=i64::MAX to 0..=u64::MAX preserving order.
    ((v as u64) ^ (1u64 << 63)).to_be_bytes()
}

/// Inverse of [`order_i64`].
fn unorder_i64(b: [u8; 8]) -> i64 {
    (u64::from_be_bytes(b) ^ (1u64 << 63)) as i64
}

fn resolution_ordinal(r: Resolution) -> u8 {
    // Stable 1-byte tag; the position in `Resolution::ALL` (ascending duration).
    Resolution::ALL
        .iter()
        .position(|&x| x == r)
        .expect("every Resolution is in Resolution::ALL") as u8
}

/// `instrument ‖ 0x00` — the series prefix shared by all of one instrument's rows.
pub fn series_prefix(instrument: &InstrumentId) -> Vec<u8> {
    let mut k = Vec::with_capacity(instrument.as_str().len() + 1);
    k.extend_from_slice(instrument.as_str().as_bytes());
    k.push(DELIM);
    k
}

/// `instrument ‖ 0x00 ‖ order(time)` — a funding/premium/futures row key.
pub fn series_key(instrument: &InstrumentId, time: Timestamp) -> Vec<u8> {
    let mut k = series_prefix(instrument);
    k.extend_from_slice(&order_i64(time.millis()));
    k
}

/// `instrument ‖ 0x00 ‖ [resolution] ‖ order(time)` — a bar row key.
pub fn bar_key(instrument: &InstrumentId, resolution: Resolution, time: Timestamp) -> Vec<u8> {
    let mut k = bar_prefix(instrument, resolution);
    k.extend_from_slice(&order_i64(time.millis()));
    k
}

/// `instrument ‖ 0x00 ‖ [resolution]` — the prefix shared by all bars of one instrument+resolution.
pub fn bar_prefix(instrument: &InstrumentId, resolution: Resolution) -> Vec<u8> {
    let mut k = series_prefix(instrument);
    k.push(resolution_ordinal(resolution));
    k
}

/// Unambiguous key for an indicator-state cache entry:
/// `u16(len sym) ‖ sym ‖ [resolution] ‖ u16(len id) ‖ id ‖ u32(lookback) ‖ order(time)`.
///
/// Components are length-prefixed (`u16`, no truncation for any realistic symbol/id) so they can't
/// collide — this key is used for exact lookups, not prefix range scans.
pub fn indicator_key(
    instrument: &InstrumentId,
    resolution: Resolution,
    indicator_id: &str,
    lookback: u32,
    time: Timestamp,
) -> Vec<u8> {
    let sym = instrument.as_str().as_bytes();
    let id = indicator_id.as_bytes();
    let mut k = Vec::with_capacity(2 + sym.len() + 1 + 2 + id.len() + 4 + 8);
    k.extend_from_slice(&(sym.len() as u16).to_be_bytes());
    k.extend_from_slice(sym);
    k.push(resolution_ordinal(resolution));
    k.extend_from_slice(&(id.len() as u16).to_be_bytes());
    k.extend_from_slice(id);
    k.extend_from_slice(&lookback.to_be_bytes());
    k.extend_from_slice(&order_i64(time.millis()));
    k
}

/// Recover the timestamp from a key whose **trailing 8 bytes** are an `order(time)` (the series and
/// bar keys; `indicator_key` also ends this way).
pub fn time_from_key(key: &[u8]) -> Timestamp {
    let n = key.len();
    let mut tail = [0u8; 8];
    tail.copy_from_slice(&key[n - 8..]);
    Timestamp::from_millis(unorder_i64(tail))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inst(s: &str) -> InstrumentId {
        InstrumentId::new(s).unwrap()
    }

    #[test]
    fn time_ordering_is_preserved_in_bytes_including_negatives() {
        let a = order_i64(-1_000);
        let b = order_i64(0);
        let c = order_i64(1_000);
        assert!(a < b && b < c, "sign-flipped BE must sort numerically");
    }

    #[test]
    fn time_round_trips_through_key() {
        let k = series_key(&inst("BTCUSDT"), Timestamp::from_millis(1_700_000_000_123));
        assert_eq!(time_from_key(&k).millis(), 1_700_000_000_123);
        let neg = bar_key(
            &inst("ETHUSDT"),
            Resolution::M5,
            Timestamp::from_millis(-42),
        );
        assert_eq!(time_from_key(&neg).millis(), -42);
    }

    #[test]
    fn prefixes_isolate_instruments_and_resolutions() {
        let btc = bar_prefix(&inst("BTCUSDT"), Resolution::M5);
        let eth = bar_prefix(&inst("ETHUSDT"), Resolution::M5);
        let btc_h1 = bar_prefix(&inst("BTCUSDT"), Resolution::H1);
        assert!(!btc.starts_with(&eth) && !eth.starts_with(&btc));
        assert_ne!(btc, btc_h1);
        // A bar key starts with its prefix.
        let k = bar_key(&inst("BTCUSDT"), Resolution::M5, Timestamp::from_secs(1));
        assert!(k.starts_with(&btc));
    }
}
