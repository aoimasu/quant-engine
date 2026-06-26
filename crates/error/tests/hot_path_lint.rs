//! Proves AC #2: clippy rejects `unwrap`/`expect`/`panic` in a hot-path
//! (`deny(clippy::unwrap_used, expect_used, panic)`) module.
//!
//! Runs `cargo clippy` against the excluded `hotpath_violation` fixture crate (which contains an
//! `unwrap()`, an `expect()`, and a `panic!()` inside such a module) and asserts the build fails
//! naming each lint. A per-run `CARGO_TARGET_DIR` (PID-scoped, cleaned up) avoids cross-run/parallel
//! contention and stale-cache false passes.

use std::path::PathBuf;
use std::process::Command;

#[test]
fn clippy_rejects_banned_constructs_in_hot_path_module() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/hotpath_violation/Cargo.toml");
    let target =
        std::env::temp_dir().join(format!("qe_error_hotpath_clippy_{}", std::process::id()));

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

    let status = output.status;
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let _ = std::fs::remove_dir_all(&target); // best-effort cleanup

    assert!(
        !status.success(),
        "clippy should FAIL on banned constructs in a deny module, but it succeeded.\n{stderr}"
    );
    for lint in ["unwrap", "expect", "panic"] {
        assert!(
            stderr.contains(lint),
            "clippy failed but did not flag `{lint}`; stderr:\n{stderr}"
        );
    }
}
