//! Stamps `GRIDFPV_BUILD_VERSION` — the version the running build *reports* (#513), read back
//! as [`gridfpv_server::BUILD_VERSION`] and served on `/about` (the console's brand stamp, the
//! Director banner, the log session header).
//!
//! The scheme, per the field ruling (#513): **a release names itself, everything else names its
//! commit.** Before this every build — the `dev-*` prerelease portables included — reported the
//! static workspace version (`0.4.0-alpha.1`), so "which build is this?" was unanswerable on a
//! support call.
//!
//! Resolution order:
//! 1. `GRIDFPV_RELEASE_VERSION` env (leading `v` stripped) — the explicit override for a build
//!    pipeline that knows exactly what it is shipping.
//! 2. HEAD exactly on a **clean** `v*` tag ⇒ the tag minus the `v` — the standard
//!    alpha/beta/full naming (`0.4.0-alpha.1`, `0.4.0`). The `--match "v*"` filter is
//!    load-bearing: the `dev-2026-*` prerelease tags are exactly the builds that must NOT name
//!    themselves like releases.
//! 3. Otherwise ⇒ `<workspace base>-dev-<short hash>`, `-dirty` appended when the tree has
//!    uncommitted changes (a dirty build is not its hash).
//! 4. No usable git at all (a source tarball) ⇒ the workspace version verbatim.

use std::process::Command;

fn main() {
    // Re-stamp when HEAD moves (commit / checkout / tag) or the index changes (staging is the
    // cheapest observable signal that the dirty state may have flipped). Both relative to the
    // workspace root's .git, two levels up from this crate.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/index");
    println!("cargo:rerun-if-env-changed=GRIDFPV_RELEASE_VERSION");

    let pkg = std::env::var("CARGO_PKG_VERSION").expect("cargo sets CARGO_PKG_VERSION");
    println!(
        "cargo:rustc-env=GRIDFPV_BUILD_VERSION={}",
        build_version(&pkg)
    );
}

fn build_version(pkg: &str) -> String {
    if let Ok(v) = std::env::var("GRIDFPV_RELEASE_VERSION") {
        let v = v.trim();
        if !v.is_empty() {
            return v.trim_start_matches('v').to_string();
        }
    }
    // `git()` yields None for empty output, so Some(porcelain) == a dirty tree.
    let dirty = git(&["status", "--porcelain"]).is_some();
    if !dirty {
        if let Some(tag) = git(&[
            "describe",
            "--exact-match",
            "--tags",
            "--match",
            "v*",
            "HEAD",
        ]) {
            return tag.trim_start_matches('v').to_string();
        }
    }
    let Some(hash) = git(&["rev-parse", "--short=7", "HEAD"]) else {
        // No git (a source tarball / a stripped checkout): the workspace version verbatim is
        // the most honest answer left.
        return pkg.to_string();
    };
    // The dev base is the workspace version's MAJOR.MINOR.PATCH — the milestone being worked
    // toward — with the `-alpha.N`-style tail dropped; the commit hash is the real identity.
    let base = pkg.split('-').next().unwrap_or(pkg);
    let dirty = if dirty { "-dirty" } else { "" };
    format!("{base}-dev-{hash}{dirty}")
}

/// Run `git` in the crate directory (git walks up to the workspace root on its own); `None` on
/// a failed/missing git or empty output, so callers read `Some` as a real answer.
fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    let s = s.trim();
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}
