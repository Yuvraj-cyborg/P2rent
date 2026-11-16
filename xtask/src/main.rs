//! Development tasks that need logic, written in Rust rather than shell.
//!
//! Run one with `cargo xtask <task>`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
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

/// The modules of `model` that are translated to Lean and proven.
/// Each holds decision logic and nothing else, written in the plain style
/// the translator accepts: loops and indexing, no iterators or closures.
const PROVEN_MODULES: &[&str] = &[
    "crate::artifact::bitfield",
    "crate::artifact::layout",
    "crate::schedule",
];

/// The Aeneas release whose `charon` and `aeneas` produce the Lean code. It
/// is also the revision of the Aeneas Lean library in `proofs/lakefile.toml`.
const AENEAS_RELEASE: &str = "nightly-2026.11.16-1dc8ad3";

/// The benchmark that counts instructions under Valgrind. See [`bench()`].
const INSTRUCTIONS_BENCH: &str = "benches/instructions.rs";

/// Every crate `model` may depend on directly. It holds the logic that gets
/// proven, so each addition here is a decision: it must not bring in a
/// disk, a network, a clock or an async runtime.
const MODEL_DEPENDENCIES: &[&str] = &["blake3", "postcard", "serde", "thiserror"];

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["size"] => finish(size()),
        ["layers"] => finish(layers()),
        ["deps"] => finish(deps(false)),
        ["deps", "--write"] => finish(deps(true)),
        ["fuzz", seconds] => finish(fuzz(seconds)),
        ["proofs"] => finish(proofs()),
        ["bench"] => finish(bench(None)),
        ["bench", base] => finish(bench(Some(base))),
        _ => {
            eprintln!(
                "usage: cargo xtask <size | layers | deps [--write] | fuzz SECONDS | proofs | bench [COMMIT]>"
            );
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
    run(cargo().args(["build", "--release", "--locked", "--bin", "mux"]))?;

    let binary = workspace_root()
        .join("target")
        .join("release")
        .join(format!("mux{}", std::env::consts::EXE_SUFFIX));
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
            "cli",
            "--target",
            target,
        ]))?;
        let names: BTreeSet<&str> = tree.lines().filter_map(third_party).collect();
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

/// The name of the crate on one line of `cargo tree`, unless it is one of
/// this workspace's own, which are printed with the folder they live in.
fn third_party(line: &str) -> Option<&str> {
    let mut words = line.split_whitespace();
    let name = words.next()?;
    let local = words.any(|word| word.starts_with('(') && word != "(proc-macro)" && word != "(*)");
    (!local).then_some(name)
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

/// Checks that `model` depends only on the crates it is allowed to.
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
        "model",
    ]))?;
    let unexpected: Vec<&str> = tree
        .lines()
        .skip(1)
        .filter_map(|line| line.split_whitespace().next())
        .filter(|name| !MODEL_DEPENDENCIES.contains(name))
        .collect();
    if unexpected.is_empty() {
        println!("model depends only on: {}", MODEL_DEPENDENCIES.join(", "));
        Ok(())
    } else {
        Err(format!(
            "model may not depend on {}; see MODEL_DEPENDENCIES in xtask/src/main.rs",
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

/// Translates the proven modules of `model` to Lean again and checks
/// every proof against the result.
///
/// The translation overwrites `proofs/Model/Types.lean` and
/// `Funs.lean`, which are committed. If they change, the Rust code changed:
/// commit them with it, so the proofs are always about the current code.
fn proofs() -> Result<(), String> {
    let root = workspace_root();
    let core = root.join("crates").join("model");
    let lean = root.join("proofs").join("Model");
    let llbc = root.join("target").join("model.llbc");
    let missing = |tool: &str| {
        format!(
            "cannot run `{tool}`. Install the Aeneas release {AENEAS_RELEASE} and put it on \
             the path: see proofs/README.md"
        )
    };

    // Rust to Charon's intermediate form.
    let mut charon = Command::new("charon");
    charon
        .current_dir(&core)
        .args(["cargo", "--preset=aeneas", "--dest-file"])
        .arg(&llbc);
    for module in PROVEN_MODULES {
        charon.args(["--start-from", module]);
    }
    // The core's own code is the same with or without its one feature.
    charon.args(["--", "--no-default-features"]);
    run(&mut charon).map_err(|_| missing("charon"))?;

    // That to Lean.
    run(Command::new("aeneas")
        .args(["-backend", "lean", "-split-files", "-dest"])
        .arg(&lean)
        .arg(&llbc))
    .map_err(|_| missing("aeneas"))?;

    // A standard-library function that neither Aeneas nor FunsExternal.lean
    // defines would be an unproven assumption. Refuse it.
    let template = lean.join("FunsExternal_Template.lean");
    if let Ok(needed) = std::fs::read_to_string(&template) {
        let models = std::fs::read_to_string(lean.join("FunsExternal.lean"))
            .map_err(|e| format!("cannot read FunsExternal.lean: {e}"))?;
        let unmodelled: Vec<&str> = needed
            .lines()
            .filter_map(|line| line.trim().strip_prefix("@[rust_fun \""))
            .filter_map(|rest| rest.split('"').next())
            .filter(|name| !models.contains(name))
            .collect();
        std::fs::remove_file(&template)
            .map_err(|e| format!("cannot remove {}: {e}", template.display()))?;
        if !unmodelled.is_empty() {
            return Err(format!(
                "the translated code calls functions with no model in FunsExternal.lean: {}",
                unmodelled.join(", ")
            ));
        }
    }

    // Check every proof.
    run(Command::new("lake")
        .arg("build")
        .current_dir(root.join("proofs")))
    .map_err(|_| "the proofs do not check, or `lake` is not installed".to_owned())
}

/// Runs the benchmarks that count instructions.
///
/// With `base`, a commit, that commit is measured first, in a worktree of
/// its own, and this tree is then compared with it. A count that rises by
/// more than the limit set in the benchmark file fails the run. Without
/// `base`, this tree is compared with its own previous run.
///
/// The counts come from Valgrind, so this needs Linux.
fn bench(base: Option<&str>) -> Result<(), String> {
    let target = workspace_root().join("target");
    // Both trees write their results here, which is what lets one be
    // compared with the other.
    let results = target.join("gungraun");

    let mut head = instructions(&results);
    if let Some(base) = base {
        let tree = target.join("bench-base");
        // Left behind if an earlier run was interrupted.
        let _ = output(git().args(["worktree", "remove", "--force"]).arg(&tree));
        run(git()
            .args(["worktree", "add", "--detach"])
            .arg(&tree)
            .arg(base))?;
        let measured = if tree.join(INSTRUCTIONS_BENCH).exists() {
            head.arg("--baseline=base");
            run(instructions(&results)
                .current_dir(&tree)
                .env("CARGO_TARGET_DIR", &target)
                .arg("--save-baseline=base"))
        } else {
            eprintln!("{base} has no instruction-count benchmark: nothing to compare with");
            Ok(())
        };
        run(git().args(["worktree", "remove", "--force"]).arg(&tree))?;
        measured?;
    }
    run(&mut head).map_err(|_| {
        "a benchmark failed or executes more instructions than its limit allows. \
         Running them needs Linux, Valgrind and gungraun-runner: see docs/performance.md"
            .to_owned()
    })
}

/// `cargo bench` for the instruction-count benchmarks, ready to take the
/// benchmark runner's own arguments.
fn instructions(results: &Path) -> Command {
    let mut command = cargo();
    command
        .args(["bench", "--locked", "--package", "benches"])
        .args(["--bench", "instructions", "--"])
        .env("GUNGRAUN_HOME", results);
    command
}

fn git() -> Command {
    let mut command = Command::new("git");
    command.current_dir(workspace_root());
    command
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
