//! Drives the real binary: one process seeds, another downloads.

use std::collections::BTreeMap;
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

/// What is added to a file's name while it is being downloaded.
const PART_SUFFIX: &str = ".mux-part";

/// Every file under `root`, by its path relative to `root`.
fn files_under(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn visit(root: &Path, dir: &Path, found: &mut BTreeMap<String, Vec<u8>>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(root, &path, found);
            } else {
                let relative = path.strip_prefix(root).unwrap();
                let name: Vec<_> = relative
                    .iter()
                    .map(|part| part.to_str().unwrap().to_owned())
                    .collect();
                found.insert(name.join("/"), std::fs::read(&path).unwrap());
            }
        }
    }
    let mut found = BTreeMap::new();
    visit(root, root, &mut found);
    found
}

/// Numbers that look random and can be repeated from their seed.
struct Dice(u64);

impl Dice {
    /// A duration somewhere between nothing and `limit`.
    fn up_to(&mut self, limit: Duration) -> Duration {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        limit.mul_f64((self.0 >> 11) as f64 / (1u64 << 53) as f64)
    }
}

/// Stops a download dead at random moments, as a crash or a power cut
/// would. Whenever it stops, every file that has its final name is complete
/// and correct, and running the download again finishes it.
#[test]
fn a_download_killed_at_any_moment_leaves_only_whole_files_and_resumes() {
    const ROUNDS: usize = 8;

    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("tree");
    std::fs::create_dir_all(source.join("nested")).unwrap();
    let data = payload();
    for (number, name) in ["a.bin", "b.bin", "nested/c.bin", "nested/d.bin"]
        .iter()
        .enumerate()
    {
        // Each file differs, and the last is cut short of a whole chunk.
        let mut contents = data.clone();
        contents.rotate_left(number * 1000 + 1);
        contents.truncate(contents.len() - number * 777);
        std::fs::write(source.join(name), contents).unwrap();
    }
    std::fs::write(source.join("empty"), b"").unwrap();
    let expected = files_under(&source);
    let (_seeder, ticket) = seed(&tmp.path().join("seed.key"), &source);
    let key = tmp.path().join("get.key");
    let quiet = |out: &Path| {
        let mut command = get(&key, &ticket, out);
        command.stdout(Stdio::null());
        command
    };

    // How long an undisturbed download takes on this machine.
    let started = Instant::now();
    assert!(quiet(&tmp.path().join("whole")).status().unwrap().success());
    let whole = started.elapsed();
    assert_eq!(
        files_under(&tmp.path().join("whole").join("tree")),
        expected
    );

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    let seed = now.as_secs() ^ (u64::from(now.subsec_nanos()) << 32);
    let mut dice = Dice(seed | 1);
    let mut cut_short = 0;
    for round in 0..ROUNDS {
        let out = tmp.path().join(format!("out-{round}"));
        // Kill it twice, so that a resumed download is interrupted too.
        for kill in 0..2 {
            let mut child = quiet(&out).spawn().unwrap();
            std::thread::sleep(dice.up_to(whole));
            child.kill().unwrap();
            child.wait().unwrap();

            let found = files_under(&out.join("tree"));
            for (name, contents) in &found {
                if name.ends_with(PART_SUFFIX) {
                    continue;
                }
                assert!(
                    expected.get(name) == Some(contents),
                    "seed {seed}, round {round}, kill {kill}: {name} has its final name \
                     but is not the whole file"
                );
            }
            if found != expected {
                cut_short += 1;
            }
        }

        assert!(
            quiet(&out).status().unwrap().success(),
            "seed {seed}, round {round}: the download did not resume"
        );
        assert!(
            files_under(&out.join("tree")) == expected,
            "seed {seed}, round {round}: the resumed download differs from the source"
        );
    }
    assert!(
        cut_short > 0,
        "seed {seed}: no download was interrupted, so nothing was tested"
    );
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
