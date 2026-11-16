//! Development tasks that need logic, written in Rust rather than shell.
//!
//! Run one with `cargo xtask <task>`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::{Command, ExitCode};

/// Largest release binary allowed. Raise it only in a pull request that says
/// what the extra bytes buy.
const MAX_BINARY_BYTES: u64 = 5 * 1024 * 1024;

/// The committed list of third-party crates that are compiled into the
/// released binaries. See [`deps`].
const SHIPPED_CRATES: &str = "xtask/shipped-crates.txt";

/// The workflow that builds the released binaries. Its target list decides
/// what "shipped" means.
const RELEASE_WORKFLOW: &str = ".github/workflows/release.yml";

/// Every crate `p2rent-core` may depend on directly. The core holds the
/// logic that gets proven, so each addition here is a decision: it must not
/// bring in a disk, a network, a clock or an async runtime.
const CORE_DEPENDENCIES: &[&str] = &["blake3", "postcard", "serde", "thiserror"];

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["size"] => finish(size()),
        ["layers"] => finish(layers()),
        ["deps"] => finish(deps(false)),
        ["deps", "--write"] => finish(deps(true)),
        ["fuzz", seconds] => finish(fuzz(seconds)),
        _ => {
            eprintln!("usage: cargo xtask <size | layers | deps [--write] | fuzz SECONDS>");
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

/// Builds the release binary and reports its size, failing if it is over
/// budget.
fn size() -> Result<(), String> {
    run(cargo().args(["build", "--release", "--locked", "--bin", "p2rent"]))?;

    let binary = workspace_root()
        .join("target")
        .join("release")
        .join(format!("p2rent{}", std::env::consts::EXE_SUFFIX));
    let bytes = std::fs::metadata(&binary)
        .map_err(|e| format!("cannot read {}: {e}", binary.display()))?
        .len();

    println!("release binary: {bytes} bytes (budget {MAX_BINARY_BYTES})");
    if bytes > MAX_BINARY_BYTES {
        return Err(format!(
            "the release binary is {bytes} bytes, over the budget of {MAX_BINARY_BYTES}"
        ));
    }
    Ok(())
}

/// Checks the committed list of shipped crates against what the released
/// binaries are built from, or rewrites it when `write` is set.
///
/// The list is one crate per line, for the targets the release workflow
/// builds. It exists so that a change which adds or drops a dependency shows
/// that crate's name in its diff. Whether a dependency is safe to ship is
/// `cargo deny`'s job, and whether the binary got bigger or slower is for
/// the size check and the benchmarks.
fn deps(write: bool) -> Result<(), String> {
    let current = shipped_crates()?;
    let path = workspace_root().join(SHIPPED_CRATES);
    if write {
        std::fs::write(&path, &current)
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        println!("wrote {SHIPPED_CRATES}");
        return Ok(());
    }

    let committed = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    if committed == current {
        println!("{SHIPPED_CRATES} is up to date");
        return Ok(());
    }
    let (old, new): (BTreeSet<&str>, BTreeSet<&str>) =
        (entries(&committed).collect(), entries(&current).collect());
    for line in old.difference(&new) {
        println!("- {line}");
    }
    for line in new.difference(&old) {
        println!("+ {line}");
    }
    Err(format!(
        "{SHIPPED_CRATES} is out of date; run `cargo xtask deps --write` and commit the result"
    ))
}

/// The lines of a shipped-crates list that name a crate.
fn entries(list: &str) -> impl Iterator<Item = &str> {
    list.lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
}

/// The shipped-crates list as it should be now.
fn shipped_crates() -> Result<String, String> {
    let targets = release_targets()?;
    // For each third-party crate, the release targets that compile it in.
    let mut users: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    for target in &targets {
        let tree = output(cargo().args([
            "tree",
            "--locked",
            "--edges",
            "normal",
            "--prefix",
            "none",
            "--format",
            "{p}",
            "--package",
            "p2rent",
            "--target",
            target,
        ]))?;
        let names: BTreeSet<&str> = tree
            .lines()
            .filter_map(|line| line.split_whitespace().next())
            // This workspace's own crates are not third-party.
            .filter(|name| !name.starts_with("p2rent"))
            .collect();
        for name in names {
            users.entry(name.to_owned()).or_default().push(target);
        }
    }

    let mut list = String::from(
        "# Third-party crates compiled into the released binaries, one per line.\n\
         # A crate that only some release targets use is followed by those targets.\n\
         # Do not edit by hand: run `cargo xtask deps --write`.\n",
    );
    for (name, used_by) in &users {
        list.push_str(name);
        if used_by.len() < targets.len() {
            list.push_str(": ");
            list.push_str(&used_by.join(", "));
        }
        list.push('\n');
    }
    Ok(list)
}

/// The targets the release workflow builds, read from the workflow itself so
/// that there is one list of them.
fn release_targets() -> Result<Vec<String>, String> {
    let path = workspace_root().join(RELEASE_WORKFLOW);
    let workflow = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let targets: Vec<String> = workflow
        .lines()
        .filter_map(|line| line.trim().strip_prefix("- target: "))
        .map(str::to_owned)
        .collect();
    if targets.is_empty() {
        return Err(format!("{RELEASE_WORKFLOW} names no release target"));
    }
    Ok(targets)
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

/// Runs every fuzz target for `seconds` each, on a nightly toolchain.
fn fuzz(seconds: &str) -> Result<(), String> {
    seconds
        .parse::<u32>()
        .map_err(|_| format!("{seconds:?} is not a number of seconds"))?;

    // cargo-fuzz assumes the platform it was built for, which is not always
    // the one it runs on, so name the target outright.
    let version = output(Command::new("rustc").arg("-vV"))?;
    let host = version
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .ok_or("`rustc -vV` did not report a host")?;

    let targets = output(nightly_fuzz().arg("list"))?;
    for target in targets.lines().filter(|line| !line.is_empty()) {
        eprintln!("fuzzing {target} for {seconds}s");
        run(nightly_fuzz()
            .args(["run", "--target", host, target, "--"])
            .arg(format!("-max_total_time={seconds}")))?;
    }
    Ok(())
}

/// `cargo fuzz` on the nightly toolchain, run in the fuzz workspace.
fn nightly_fuzz() -> Command {
    // Through rustup, not `cargo +nightly`: inside `cargo run` the `cargo`
    // on the path can be the real one, which does not understand `+nightly`.
    let mut command = Command::new("rustup");
    command
        .args(["run", "nightly", "cargo", "fuzz"])
        .current_dir(workspace_root().join("fuzz"));
    command
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
        return Err(format!(
            "{command:?} failed with {}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim_end()
        ));
    }
    String::from_utf8(output.stdout).map_err(|e| format!("{command:?} printed invalid UTF-8: {e}"))
}
