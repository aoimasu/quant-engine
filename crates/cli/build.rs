//! Build-time code provenance (QE-420).
//!
//! Stamps the git commit the binary was built from into `QE_BUILD_GIT_SHA` so a vintage's
//! `code_commit` (folded into its lineage id) reflects the *real* source tree rather than a constant
//! crate version. Zero external dependencies by design — it only shells out to the system `git` — so
//! it adds nothing to `Cargo.lock`, the license/advisory `deny` gate, or the architectural
//! dependency/firewall guards.
//!
//! Resolution here is a *fallback*: the runtime still honours a `QE_CODE_COMMIT` override first
//! (see `src/main.rs::code_commit`). If `git` is absent or this is not a repository (e.g. a Docker
//! build context that ships no `.git`), the sentinel `"unknown"` is stamped and the runtime falls back
//! to the crate version.

use std::process::Command;

fn main() {
    // Re-stamp when HEAD moves (new commit / checkout). These paths may not exist (no `.git` in a
    // Docker build context) — cargo simply ignores rerun triggers for missing files.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/packed-refs");

    println!("cargo:rustc-env=QE_BUILD_GIT_SHA={}", build_git_sha());
}

/// The 12-char short SHA of `HEAD`, suffixed `-dirty` when the working tree has uncommitted changes.
/// Returns `"unknown"` when git is unavailable or this is not a repository.
fn build_git_sha() -> String {
    let sha = match git(&["rev-parse", "--short=12", "HEAD"]) {
        Some(s) if !s.is_empty() => s,
        _ => return "unknown".to_owned(),
    };
    // `git status --porcelain` prints one line per change; empty output == clean tree.
    let dirty = git(&["status", "--porcelain"]).is_some_and(|s| !s.is_empty());
    if dirty {
        format!("{sha}-dirty")
    } else {
        sha
    }
}

/// Run `git <args>` and return its trimmed stdout, or `None` if git is missing or exits non-zero.
fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}
