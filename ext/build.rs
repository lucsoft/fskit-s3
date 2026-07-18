//! Compile the current git SHA into the extension as `FSKIT_S3_GIT_SHA`, so it
//! can log which build is actually running at `activate` (the one signal that
//! reveals daemon-cache staleness: the right bundle on disk but an old loaded
//! process). The host reads the same SHA from the bundle's Info.plist to check
//! host/extension match — see scripts/stamp-git-sha.sh.

use std::process::Command;

fn main() {
    // Xcode's build phase exports FSKIT_S3_GIT_SHA (so Xcode builds — including a
    // dirty tree — always stamp the current value). Standalone `cargo build`
    // falls back to asking git directly.
    let sha = std::env::var("FSKIT_S3_GIT_SHA")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(git_describe)
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=FSKIT_S3_GIT_SHA={sha}");

    // Rebuild when the env override changes (Xcode path)...
    println!("cargo:rerun-if-env-changed=FSKIT_S3_GIT_SHA");
    // ...and when HEAD moves (standalone path). In a worktree `.git` is a file,
    // so ask git for the real HEAD path rather than hardcoding `.git/HEAD`.
    if let Some(head) = git_path("HEAD") {
        println!("cargo:rerun-if-changed={head}");
    }
}

fn git_describe() -> Option<String> {
    let out = Command::new("git")
        .args(["describe", "--always", "--dirty", "--abbrev=12"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

fn git_path(name: &str) -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", "--git-path", name])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}
