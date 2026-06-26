//! Proves AC #2: clippy rejects `unwrap()` in a hot-path (`deny(clippy::unwrap_used)`) module.
//!
//! Runs `cargo clippy` against the excluded `hotpath_violation` fixture crate (which contains an
//! `unwrap()` inside a `#![deny(clippy::unwrap_used)]` module) and asserts the build fails with the
//! `unwrap_used` lint. A fresh `CARGO_TARGET_DIR` avoids stale-cache false passes.

use std::path::PathBuf;
use std::process::Command;

#[test]
fn clippy_rejects_unwrap_in_hot_path_module() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/hotpath_violation/Cargo.toml");
    let target = std::env::temp_dir().join("qe_error_hotpath_clippy_target");

    let output = Command::new(env!("CARGO"))
        .args([
            "clippy",
            "--manifest-path",
            manifest.to_str().expect("manifest path utf8"),
            "--quiet",
        ])
        .env("CARGO_TARGET_DIR", &target)
        .output()
        .expect("failed to run `cargo clippy` on the fixture");

    assert!(
        !output.status.success(),
        "clippy should FAIL on an unwrap() inside a deny(unwrap_used) module, but it succeeded"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unwrap_used") || stderr.contains("used `unwrap()`"),
        "clippy failed but not for the expected lint; stderr:\n{stderr}"
    );
}
