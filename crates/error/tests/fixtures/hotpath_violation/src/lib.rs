//! Fixture: a hot-path-annotated module exercising all three banned constructs. `cargo clippy`
//! must reject each. Excluded from the workspace; compiled only by `tests/hot_path_lint.rs`.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

/// Triggers `clippy::unwrap_used`.
pub fn v_unwrap() -> i32 {
    Some(1).unwrap()
}

/// Triggers `clippy::expect_used`.
pub fn v_expect() -> i32 {
    Some(1).expect("must be present")
}

/// Triggers `clippy::panic`.
pub fn v_panic() -> i32 {
    panic!("deliberate")
}
