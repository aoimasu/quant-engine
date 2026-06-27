//! Schema-drift detection across months.
//!
//! Binance occasionally changes a dump's CSV columns. We extract the header row of each archive and
//! compare it against the first header seen for that [`DataKind`]; a later month whose columns
//! differ is flagged so fusion (QE-104) never silently mis-maps a column.

use std::collections::HashMap;
use std::io::{Cursor, Read};

use crate::source::DataKind;
use crate::IngestError;

/// The result of comparing an observed header against a baseline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriftStatus {
    /// Columns are identical (same names, same order).
    InSync,
    /// Columns differ.
    Drift {
        /// Columns present now but not in the baseline.
        added: Vec<String>,
        /// Baseline columns no longer present.
        removed: Vec<String>,
        /// Same set of columns but a different order.
        reordered: bool,
    },
}

/// Compare an `observed` header against a `baseline` one.
#[must_use]
pub fn detect_drift(baseline: &[String], observed: &[String]) -> DriftStatus {
    if baseline == observed {
        return DriftStatus::InSync;
    }
    let added: Vec<String> = observed
        .iter()
        .filter(|c| !baseline.contains(c))
        .cloned()
        .collect();
    let removed: Vec<String> = baseline
        .iter()
        .filter(|c| !observed.contains(c))
        .cloned()
        .collect();
    // Same multiset of names but different order → purely reordered.
    let reordered = added.is_empty() && removed.is_empty();
    DriftStatus::Drift {
        added,
        removed,
        reordered,
    }
}

/// Read the header row (first line, comma-split, trimmed) of the first entry in a ZIP archive.
///
/// # Errors
/// [`IngestError::Archive`] if the bytes are not a readable ZIP, are empty, or the entry is empty.
pub fn csv_header(zip_bytes: &[u8]) -> Result<Vec<String>, IngestError> {
    let mut archive = zip::ZipArchive::new(Cursor::new(zip_bytes))
        .map_err(|e| IngestError::Archive(e.to_string()))?;
    if archive.is_empty() {
        return Err(IngestError::Archive("empty archive".to_owned()));
    }
    let mut entry = archive
        .by_index(0)
        .map_err(|e| IngestError::Archive(e.to_string()))?;
    let mut contents = String::new();
    entry
        .read_to_string(&mut contents)
        .map_err(|e| IngestError::Archive(e.to_string()))?;
    let first = contents
        .lines()
        .next()
        .ok_or_else(|| IngestError::Archive("empty CSV".to_owned()))?;
    Ok(first.split(',').map(|c| c.trim().to_owned()).collect())
}

/// Records the first header seen per [`DataKind`] and flags later differing months.
#[derive(Debug, Default)]
pub struct SchemaRegistry {
    baselines: HashMap<String, Vec<String>>,
}

impl SchemaRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Check `header` for `kind`: the first header seen becomes the baseline ([`DriftStatus::InSync`]);
    /// later headers are compared against it.
    pub fn check(&mut self, kind: DataKind, header: &[String]) -> DriftStatus {
        let key = format!("{kind:?}");
        match self.baselines.get(&key) {
            Some(baseline) => detect_drift(baseline, header),
            None => {
                self.baselines.insert(key, header.to_vec());
                DriftStatus::InSync
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qe_domain::Resolution;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn cols(cs: &[&str]) -> Vec<String> {
        cs.iter().map(|s| (*s).to_owned()).collect()
    }

    /// Build an in-memory ZIP containing one CSV file with the given content.
    fn zip_with_csv(name: &str, content: &str) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut w = zip::ZipWriter::new(Cursor::new(&mut buf));
            w.start_file(name, SimpleFileOptions::default()).unwrap();
            w.write_all(content.as_bytes()).unwrap();
            w.finish().unwrap();
        }
        buf
    }

    #[test]
    fn identical_headers_are_in_sync() {
        let h = cols(&["open_time", "open", "close"]);
        assert_eq!(detect_drift(&h, &h), DriftStatus::InSync);
    }

    #[test]
    fn detects_added_removed_and_reordered() {
        let base = cols(&["a", "b", "c"]);
        assert_eq!(
            detect_drift(&base, &cols(&["a", "b", "c", "d"])),
            DriftStatus::Drift {
                added: cols(&["d"]),
                removed: vec![],
                reordered: false,
            }
        );
        assert_eq!(
            detect_drift(&base, &cols(&["a", "c"])),
            DriftStatus::Drift {
                added: vec![],
                removed: cols(&["b"]),
                reordered: false,
            }
        );
        assert_eq!(
            detect_drift(&base, &cols(&["c", "b", "a"])),
            DriftStatus::Drift {
                added: vec![],
                removed: vec![],
                reordered: true,
            }
        );
    }

    #[test]
    fn reads_header_from_zip() {
        let zip = zip_with_csv(
            "BTCUSDT-5m-2020-01-07.csv",
            "open_time,open,high,low,close\n1,2,3,4,5\n",
        );
        assert_eq!(
            csv_header(&zip).unwrap(),
            cols(&["open_time", "open", "high", "low", "close"])
        );
        assert!(matches!(
            csv_header(b"not a zip"),
            Err(IngestError::Archive(_))
        ));
    }

    #[test]
    fn registry_baselines_then_flags_later_drift() {
        let mut reg = SchemaRegistry::new();
        let kind = DataKind::Klines(Resolution::M5);
        // First month → baseline, in sync.
        assert_eq!(
            reg.check(kind, &cols(&["open_time", "open", "close"])),
            DriftStatus::InSync
        );
        // Same columns next month → in sync.
        assert_eq!(
            reg.check(kind, &cols(&["open_time", "open", "close"])),
            DriftStatus::InSync
        );
        // A new column → drift.
        assert!(matches!(
            reg.check(kind, &cols(&["open_time", "open", "close", "ignore"])),
            DriftStatus::Drift { .. }
        ));
    }
}
