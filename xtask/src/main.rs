//! Development tasks that need logic, written in Rust rather than shell.
//!
//! Run one with `cargo xtask <task>`.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

/// Largest release binary allowed. Raise it only in a pull request that says
/// what the extra bytes buy.
const MAX_BINARY_BYTES: u64 = 5 * 1024 * 1024;

/// Most third-party crates allowed in the dependency tree of the shipped
/// binary, counted across every platform so that the number is the same
/// whichever machine runs the check. Raise it only in a pull request that
/// names the new dependencies and why they are needed.
const MAX_DEPENDENCIES: usize = 130;

/// Every crate `p2rent-core` may depend on directly. The core holds the
/// logic that gets proven, so each addition here is a decision: it must not
/// bring in a disk, a network, a clock or an async runtime.
const CORE_DEPENDENCIES: &[&str] = &["blake3", "postcard", "serde", "thiserror"];

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        Some("size") => finish(size()),
        Some("layers") => finish(layers()),
        _ => {
            eprintln!("usage: cargo xtask <size|layers>");
            ExitCode::from(2)
        }
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
        "--target",
        "all",
        "--package",
        "p2rent",
    ]))?;
    // This workspace's own crates are not dependencies in the sense meant
    // here.
    let crates: BTreeSet<&str> = tree
        .lines()
        .map(|line| line.trim_end_matches(" (*)"))
        .filter(|line| !line.is_empty() && !line.starts_with("p2rent"))
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

/// Checks that `p2rent-core` depends only on the crates it is allowed to.
fn layers() -> Result<(), String> {
    let tree = output(cargo().args([
        "tree",
        "--locked",
        "--edges",
        "normal",
        "--depth",
        "1",
        "--prefix",
        "none",
        "--package",
        "p2rent-core",
    ]))?;
    let unexpected: Vec<&str> = tree
        .lines()
        .skip(1)
        .filter_map(|line| line.split_whitespace().next())
        .filter(|name| !CORE_DEPENDENCIES.contains(name))
        .collect();
    if unexpected.is_empty() {
        println!(
            "p2rent-core depends only on: {}",
            CORE_DEPENDENCIES.join(", ")
        );
        Ok(())
    } else {
        Err(format!(
            "p2rent-core may not depend on {}; see CORE_DEPENDENCIES in xtask/src/main.rs",
            unexpected.join(", ")
        ))
    }
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
