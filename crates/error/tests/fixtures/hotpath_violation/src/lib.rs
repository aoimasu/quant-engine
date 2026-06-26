//! Fixture: a hot-path-annotated module containing an `unwrap()`. `cargo clippy` must reject it.
//! Excluded from the workspace; compiled only by `tests/hot_path_lint.rs`.
#![deny(clippy::unwrap_used)]

/// Deliberately calls `unwrap()` inside a `deny(unwrap_used)` module — clippy should error.
#[must_use]
pub fn violates() -> i32 {
    Some(1).unwrap()
}
