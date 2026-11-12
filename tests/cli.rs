//! Drives the real binary: one process seeds, another downloads.

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_p2rent");

fn p2rent(key: &Path) -> Command {
    let mut command = Command::new(BIN);
    command.env("P2RENT_KEY", key).stdin(Stdio::null());
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
        let output = p2rent(&key).arg("id").output().unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap()
    };
    let first = id();
    assert_eq!(first.trim().len(), 64);
    assert_eq!(first, id());
}

#[test]
fn seed_then_get() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("payload.bin");
    let data: Vec<u8> = (0..3_000_000u32).map(|i| (i ^ (i >> 9)) as u8).collect();
    std::fs::write(&source, &data).unwrap();

    let mut seeder = KillOnDrop(
        p2rent(&tmp.path().join("seed.key"))
            .args(["seed", "--listen", "127.0.0.1:0", "--chunk-size", "64K"])
            .arg(&source)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut ticket = String::new();
    BufReader::new(seeder.0.stdout.take().unwrap())
        .read_line(&mut ticket)
        .unwrap();
    let ticket = ticket.trim();
    assert!(
        ticket.starts_with("p2rent://"),
        "unexpected output: {ticket:?}"
    );

    let out = tmp.path().join("out");
    let status = p2rent(&tmp.path().join("get.key"))
        .args(["get", ticket, "--listen", "127.0.0.1:0", "--out"])
        .arg(&out)
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(std::fs::read(out.join("payload.bin")).unwrap(), data);
}

#[test]
fn a_bad_ticket_is_a_usage_error() {
    let tmp = tempfile::tempdir().unwrap();
    let output = p2rent(&tmp.path().join("node.key"))
        .args(["get", "not-a-ticket"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("ticket"), "{stderr}");
}
