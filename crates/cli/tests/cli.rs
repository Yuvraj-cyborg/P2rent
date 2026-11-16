//! Drives the real binary: one process seeds, another downloads.

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_mux");

fn mux(key: &Path) -> Command {
    let mut command = Command::new(BIN);
    command.env("MUX_KEY", key).stdin(Stdio::null());
    command
}

/// Kills the child when the test ends, however it ends.
struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn id_is_stable_across_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let key = tmp.path().join("node.key");
    let id = || {
        let output = mux(&key).arg("id").output().unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap()
    };
    let first = id();
    assert_eq!(first.trim().len(), 64);
    assert_eq!(first, id());
}

/// Deterministic test data that does not compress or repeat.
fn payload() -> Vec<u8> {
    (0..3_000_000u32)
        .map(|i| (i ^ (i >> 9)).to_le_bytes()[0])
        .collect()
}

/// Seeds `source` from a new process and returns it with its ticket.
fn seed(key: &Path, source: &Path) -> (KillOnDrop, String) {
    let mut seeder = KillOnDrop(
        mux(key)
            .args(["seed", "--listen", "127.0.0.1:0", "--chunk-size", "64K"])
            .arg(source)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut ticket = String::new();
    BufReader::new(seeder.0.stdout.take().unwrap())
        .read_line(&mut ticket)
        .unwrap();
    let ticket = ticket.trim().to_owned();
    assert!(
        ticket.starts_with("mux://"),
        "unexpected output: {ticket:?}"
    );
    (seeder, ticket)
}

/// A `get` command for `ticket` that writes into `out`.
fn get(key: &Path, ticket: &str, out: &Path) -> Command {
    let mut command = mux(key);
    command
        .args(["get", ticket, "--listen", "127.0.0.1:0", "--out"])
        .arg(out)
        .stderr(Stdio::null());
    command
}

#[test]
fn seed_then_get() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("payload.bin");
    let data = payload();
    std::fs::write(&source, &data).unwrap();
    let (_seeder, ticket) = seed(&tmp.path().join("seed.key"), &source);

    let out = tmp.path().join("out");
    let status = get(&tmp.path().join("get.key"), &ticket, &out)
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(std::fs::read(out.join("payload.bin")).unwrap(), data);
}

/// Three processes: a seeder, a downloader that keeps serving, and a second
/// downloader that arrives while both are up.
#[test]
fn a_second_downloader_joins_a_running_swarm() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("payload.bin");
    let data = payload();
    std::fs::write(&source, &data).unwrap();
    let (_seeder, ticket) = seed(&tmp.path().join("seed.key"), &source);

    let first = tmp.path().join("first");
    let _first = KillOnDrop(
        get(&tmp.path().join("first.key"), &ticket, &first)
            .arg("--seed")
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(60);
    while std::fs::read(first.join("payload.bin")).ok().as_ref() != Some(&data) {
        assert!(
            Instant::now() < deadline,
            "the first download did not finish"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    let second = tmp.path().join("second");
    let status = get(&tmp.path().join("second.key"), &ticket, &second)
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(std::fs::read(second.join("payload.bin")).unwrap(), data);
}

#[test]
fn a_bad_ticket_is_a_usage_error() {
    let tmp = tempfile::tempdir().unwrap();
    let output = mux(&tmp.path().join("node.key"))
        .args(["get", "not-a-ticket"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("ticket"), "{stderr}");
}
