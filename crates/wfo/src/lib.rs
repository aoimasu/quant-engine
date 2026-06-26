//! qe-wfo — walk-forward optimisation (QD MAP-Elites).
//!
//! Scaffold crate established in QE-001; real APIs land in later tickets.

/// Returns this crate's package name. Placeholder until later tickets add real APIs.
#[must_use]
pub fn crate_name() -> &'static str {
    "qe-wfo"
}

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_set() {
        assert_eq!(super::crate_name(), "qe-wfo");
    }
}
