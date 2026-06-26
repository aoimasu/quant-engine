//! Reproducibility harness: re-run a stage and assert byte-identical artefacts.
//!
//! A stage is *reproducible* when, given the same lineage (seed + inputs), two independent
//! executions emit the same bytes. [`reproduce`] is the literal "re-run twice and compare" check
//! from QE-006, reusable from any stage's own test suite.

use thiserror::Error;

/// Run `stage` twice and return its output iff both runs are byte-identical.
///
/// `stage` is `FnMut` so it may carry per-call setup, but a *reproducible* stage must still emit
/// identical bytes on each call.
///
/// # Errors
/// Returns [`ReproError`] (both runs' lengths + first differing offset) when the runs disagree.
pub fn reproduce<F>(mut stage: F) -> Result<Vec<u8>, ReproError>
where
    F: FnMut() -> Vec<u8>,
{
    let first = stage();
    let second = stage();
    if first == second {
        Ok(first)
    } else {
        Err(ReproError::new(&first, &second))
    }
}

/// `true` iff `stage` produces byte-identical output across two runs.
pub fn is_reproducible<F>(stage: F) -> bool
where
    F: FnMut() -> Vec<u8>,
{
    reproduce(stage).is_ok()
}

/// Two runs of a stage produced different bytes.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error(
    "stage is not reproducible: run lengths {first_len} vs {second_len}, first difference at offset {first_diff:?}"
)]
pub struct ReproError {
    /// Byte length of the first run's output.
    pub first_len: usize,
    /// Byte length of the second run's output.
    pub second_len: usize,
    /// Offset of the first differing byte, or the shorter length when one is a prefix of the other.
    pub first_diff: Option<usize>,
}

impl ReproError {
    fn new(first: &[u8], second: &[u8]) -> Self {
        let first_diff = first
            .iter()
            .zip(second)
            .position(|(a, b)| a != b)
            .or_else(|| (first.len() != second.len()).then_some(first.len().min(second.len())));
        Self {
            first_len: first.len(),
            second_len: second.len(),
            first_diff,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_stage_reproduces() {
        let out = reproduce(|| vec![1, 2, 3]).expect("stable");
        assert_eq!(out, vec![1, 2, 3]);
        assert!(is_reproducible(|| vec![9; 4]));
    }

    #[test]
    fn diverging_stage_reports_first_offset() {
        let mut n = 0u8;
        let err = reproduce(|| {
            n += 1;
            vec![0, 0, n]
        })
        .unwrap_err();
        assert_eq!(err.first_diff, Some(2));
        assert_eq!((err.first_len, err.second_len), (3, 3));
    }

    #[test]
    fn length_mismatch_reports_shorter_len() {
        let mut calls = 0usize;
        let err = reproduce(|| {
            calls += 1;
            vec![7u8; calls] // 1 byte, then 2 bytes: a prefix, differing only in length
        })
        .unwrap_err();
        assert_eq!(err.first_diff, Some(1));
        assert_eq!((err.first_len, err.second_len), (1, 2));
    }
}
