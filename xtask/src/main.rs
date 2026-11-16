//! Development tasks that need logic, written in Rust rather than shell.
//!
//! Run one with `cargo xtask <task>`.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

/// Largest release binary allowed. Raise it only in a pull request that says
/// what the extra bytes buy.
const MAX_BINARY_BYTES: u64 = 5 * 1024 * 1024;

/// Most crates allowed in the dependency tree of the shipped binary. Raise it
/// only in a pull request that names the new dependencies and why they are
/// needed.
const MAX_DEPENDENCIES: usize = 110;

fn main() -> ExitCode {
    if std::env::args().nth(1).as_deref() == Some("size") {
        finish(size())
    } else {
        eprintln!("usage: cargo xtask size");
        ExitCode::from(2)
    }
}

fn finish(outcome: Result<(), String>) -> ExitCode {
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(reason) => {
            eprintln!("error: {reason}");
            ExitCode::FAILURE
        }
    }
}

/// Builds the release binary and reports its size and dependency count as a
/// Markdown table, failing if either is over budget.
fn size() -> Result<(), String> {
    run(cargo().args(["build", "--release", "--locked", "--bin", "p2rent"]))?;

    let binary = workspace_root()
        .join("target")
        .join("release")
        .join(format!("p2rent{}", std::env::consts::EXE_SUFFIX));
    let bytes = std::fs::metadata(&binary)
        .map_err(|e| format!("cannot read {}: {e}", binary.display()))?
        .len();

    let tree = output(cargo().args([
        "tree",
        "--locked",
        "--edges",
        "normal",
        "--prefix",
        "none",
        "--package",
        "p2rent",
    ]))?;
    let crates: BTreeSet<&str> = tree
        .lines()
        .map(|line| line.trim_end_matches(" (*)"))
        .filter(|line| !line.is_empty() && !line.starts_with("p2rent "))
        .collect();

    println!("| Measure | Now | Budget |");
    println!("| --- | ---: | ---: |");
    println!("| Release binary (bytes) | {bytes} | {MAX_BINARY_BYTES} |");
    println!("| Dependencies | {} | {MAX_DEPENDENCIES} |", crates.len());

    if bytes > MAX_BINARY_BYTES {
        return Err(format!(
            "the release binary is {bytes} bytes, over the budget of {MAX_BINARY_BYTES}"
        ));
    }
    if crates.len() > MAX_DEPENDENCIES {
        return Err(format!(
            "{} dependencies, over the budget of {MAX_DEPENDENCIES}",
            crates.len()
        ));
    }
    Ok(())
}

fn cargo() -> Command {
    let mut command = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    command.current_dir(workspace_root());
    command
}

fn workspace_root() -> PathBuf {
    // This file lives in `<root>/xtask`.
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn run(command: &mut Command) -> Result<(), String> {
    let status = command
        .status()
        .map_err(|e| format!("cannot start {command:?}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{command:?} failed with {status}"))
    }
}

fn output(command: &mut Command) -> Result<String, String> {
    let output = command
        .output()
        .map_err(|e| format!("cannot start {command:?}: {e}"))?;
    if !output.status.success() {
        return Err(format!("{command:?} failed with {}", output.status));
    }
    String::from_utf8(output.stdout).map_err(|e| format!("{command:?} printed invalid UTF-8: {e}"))
}
