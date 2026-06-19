//! GridFPV task runner — the single definition of our checks.
//!
//! Local dev runs `cargo xtask ci`; GitLab CI runs the identical `cargo xtask ci`
//! (see `.gitlab-ci.yml`). Keeping the logic here means local and remote can
//! never drift. Pure std + cargo, so it works the same on Windows/Linux/macOS.
#![forbid(unsafe_code)]

use std::process::{Command, exit};

/// Run a command, echoing it first; returns whether it succeeded.
fn run(program: &str, args: &[&str]) -> bool {
    println!("\n\x1b[1m$ {program} {}\x1b[0m", args.join(" "));
    match Command::new(program).args(args).status() {
        Ok(status) => status.success(),
        Err(err) => {
            eprintln!("failed to launch `{program}`: {err}");
            false
        }
    }
}

fn fmt() -> bool {
    run("cargo", &["fmt", "--all", "--", "--check"])
}

fn lint() -> bool {
    run(
        "cargo",
        &[
            "clippy",
            "--all-targets",
            "--all-features",
            "--",
            "-D",
            "warnings",
        ],
    )
}

fn test() -> bool {
    run("cargo", &["test", "--all"])
}

/// Rust→TS generation drift check. Wired in #4; a no-op placeholder until then.
fn generate() -> bool {
    println!("\ngen: Rust→TS generation not wired yet (#4) — skipping");
    true
}

fn main() {
    let task = std::env::args().nth(1).unwrap_or_else(|| "ci".to_string());
    let ok = match task.as_str() {
        "fmt" => fmt(),
        "lint" | "clippy" => lint(),
        "test" => test(),
        "gen" => generate(),
        "ci" => fmt() && lint() && test() && generate(),
        other => {
            eprintln!("unknown task: {other}");
            eprintln!("usage: cargo xtask [ci|fmt|lint|test|gen]");
            false
        }
    };
    if !ok {
        exit(1);
    }
}
