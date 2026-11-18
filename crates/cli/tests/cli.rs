//! Drives the real binary: one process seeds, another downloads.

mod kills;

use kills::Moments;
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

#[test]
fn protocols_lists_what_this_build_can_use_and_needs_no_key() {
    let tmp = tempfile::tempdir().unwrap();
    let key = tmp.path().join("node.key");
    let output = mux(&key).arg("protocols").output().unwrap();
    assert!(output.status.success());

    let table = String::from_utf8(output.stdout).unwrap();
    let rows: Vec<Vec<&str>> = table
        .lines()
        .map(|line| line.split_whitespace().take(3).collect())
        .collect();
    let mut expected = vec![["SCHEME", "READ", "WRITE"], ["file", "yes", "yes"]];
    if cfg!(feature = "web") {
        expected.extend([["http", "yes", "no"], ["https", "yes", "no"]]);
    }
    expected.extend([["mux", "yes", "no"], ["peer", "no", "yes"]]);
    assert_eq!(rows, expected);
    // Listing protocols is no reason to give this machine a key.
    assert!(!key.exists());
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
            .args([
                "seed",
                "--no-relay",
                "--listen",
                "127.0.0.1:0",
                "--chunk-size",
                "64K",
            ])
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
        .args([
            "get",
            ticket,
            "--no-relay",
            "--listen",
            "127.0.0.1:0",
            "--out",
        ])
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

    // How long an undisturbed download takes on this machine. The first one
    // pays for what the later ones do not: the key is made, and the program
    // is read from disk. Timed against it, most kills would come after a
    // later download was already done, so the second is the one timed.
    assert!(quiet(&tmp.path().join("first")).status().unwrap().success());
    let started = Instant::now();
    assert!(quiet(&tmp.path().join("whole")).status().unwrap().success());
    let whole = started.elapsed();
    assert_eq!(
        files_under(&tmp.path().join("whole").join("tree")),
        expected
    );

    let mut moments = Moments::new(ROUNDS, whole);
    while let Some(round) = moments.round() {
        let out = tmp.path().join(format!("out-{round}"));
        // Kill it twice, so that a resumed download is interrupted too.
        for kill in 0..2 {
            let after = moments.next();
            kills::kill(&mut quiet(&out), after);

            let found = files_under(&out.join("tree"));
            for (name, contents) in &found {
                if name.ends_with(PART_SUFFIX) {
                    continue;
                }
                assert!(
                    expected.get(name) == Some(contents),
                    "round {round}, killed after {after:?}: {name} has its final name but \
                     is not the whole file"
                );
            }
            if kill == 0 {
                moments.first_kill(found != expected);
            }
        }

        assert!(
            quiet(&out).status().unwrap().success(),
            "round {round}: the download did not resume"
        );
        assert!(
            files_under(&out.join("tree")) == expected,
            "round {round}: the resumed download differs from the source"
        );
    }
}

/// A link that stays inside the tree is sent. One that points outside it is
/// left out, and `seed` says so.
#[cfg(unix)]
#[test]
fn seed_says_what_it_left_out() {
    use std::io::Read;
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("tree");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("a.bin"), b"data").unwrap();
    std::os::unix::fs::symlink("a.bin", source.join("inside")).unwrap();
    std::os::unix::fs::symlink("/etc/hosts", source.join("outside")).unwrap();

    let mut seeder = KillOnDrop(
        mux(&tmp.path().join("seed.key"))
            .args(["seed", "--no-relay", "--listen", "127.0.0.1:0"])
            .arg(&source)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    // The ticket is printed once indexing is done, after any note.
    let mut ticket = String::new();
    BufReader::new(seeder.0.stdout.take().unwrap())
        .read_line(&mut ticket)
        .unwrap();
    assert!(
        ticket.starts_with("mux://"),
        "unexpected output: {ticket:?}"
    );
    seeder.0.kill().unwrap();
    let mut said = String::new();
    seeder
        .0
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut said)
        .unwrap();

    assert!(said.contains("note: left out 1 entry."), "{said}");
    assert!(said.contains("Links that point outside the tree"), "{said}");
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

// Another machine through `cp` and `probe`: `peer:` offers, and a ticket is
// a place to copy from.

/// Offers `source` with `mux cp SOURCE peer:` from a new process, and
/// returns it with the ticket it wrote.
fn offer(key: &Path, source: &Path) -> (KillOnDrop, String) {
    let mut offering = KillOnDrop(
        mux(key)
            .arg("cp")
            .arg(source)
            .args(["peer:", "--no-relay", "--listen", "127.0.0.1:0"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut ticket = String::new();
    BufReader::new(offering.0.stdout.take().unwrap())
        .read_line(&mut ticket)
        .unwrap();
    let ticket = ticket.trim().to_owned();
    assert!(
        ticket.starts_with("mux://"),
        "unexpected output: {ticket:?}"
    );
    (offering, ticket)
}

/// A command that has to do with the machines of `ticket`, and reaches
/// them at their addresses only.
fn with_ticket<const N: usize>(key: &Path, before: [&str; N], ticket: &str) -> Command {
    let mut command = mux(key);
    command
        .args(before)
        .arg(ticket)
        .args(["--no-relay", "--listen", "127.0.0.1:0"]);
    command
}

/// Checks the one line a copy writes on standard output: the path that was
/// written, and then `rest`. The path is the destination as it was typed
/// with the name after it, which on Windows can mix the two ways of writing
/// a path, so it is compared as a path and not as text.
fn assert_result(stdout: &[u8], path: &Path, rest: &str) {
    let line = String::from_utf8_lossy(stdout);
    let (written, said) = line.trim_end().split_once('\t').unwrap();
    assert_eq!(Path::new(written), path);
    assert_eq!(said, rest);
}

/// A small tree: two files of more than a chunk, one in a folder, and an
/// empty one.
fn small_tree(at: &Path) {
    std::fs::create_dir_all(at.join("nested")).unwrap();
    let data = payload();
    std::fs::write(at.join("a.bin"), &data[..1_500_000]).unwrap();
    std::fs::write(at.join("nested/b.bin"), &data[1_500_000..]).unwrap();
    std::fs::write(at.join("empty"), b"").unwrap();
}

#[test]
fn a_folder_is_offered_and_copied_between_machines_with_cp() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("tree");
    small_tree(&source);
    let (_offering, ticket) = offer(&tmp.path().join("offer.key"), &source);

    let key = tmp.path().join("copy.key");
    let out = tmp.path().join("out");
    let into = format!("{}/", out.display());
    let copy = || {
        with_ticket(&key, ["cp"], &ticket)
            .arg(&into)
            .output()
            .unwrap()
    };
    let copied = copy();
    let said = String::from_utf8_lossy(&copied.stderr).into_owned();
    assert!(copied.status.success(), "{said}");
    assert_eq!(files_under(&out.join("tree")), files_under(&source));
    // What was written, its size and how far it was checked, and nothing
    // else on standard output.
    assert_result(&copied.stdout, &out.join("tree"), "3000000\tverified");
    assert!(said.contains("from 1 machine"), "{said}");

    // Copied again, everything is there already.
    let again = copy();
    let said = String::from_utf8_lossy(&again.stderr).into_owned();
    assert!(again.status.success(), "{said}");
    assert!(said.contains("that was already there"), "{said}");
    assert!(!said.contains("fetched"), "{said}");
}

#[test]
fn a_copy_from_a_ticket_is_told_to_a_program_line_by_line() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("tree");
    small_tree(&source);
    let (_offering, ticket) = offer(&tmp.path().join("offer.key"), &source);

    let out = tmp.path().join("out");
    let into = format!("{}/", out.display());
    let copied = with_ticket(&tmp.path().join("copy.key"), ["--json", "cp"], &ticket)
        .arg(&into)
        .output()
        .unwrap();
    assert!(
        copied.status.success(),
        "{}",
        String::from_utf8_lossy(&copied.stderr)
    );
    let lines: Vec<serde_json::Value> = String::from_utf8(copied.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let events: Vec<&str> = lines
        .iter()
        .map(|line| line["event"].as_str().unwrap())
        .collect();

    // It begins by saying what there is to copy, and ends with what was
    // done. In between, each file that had bytes to fetch is named once,
    // as it becomes whole.
    let (first, last) = (&lines[0], &lines[lines.len() - 1]);
    assert_eq!(first["event"], "start");
    assert_eq!(
        (&first["bytes"], &first["files"]),
        (&3_000_000.into(), &3.into())
    );
    assert_eq!(first["source"], ticket.as_str());
    assert_eq!(last["event"], "done");
    assert_eq!(last["checked"], "verified");
    assert_eq!(
        (&last["fetched"], &last["kept"]),
        (&3_000_000.into(), &0.into())
    );
    assert_eq!(Path::new(last["path"].as_str().unwrap()), out.join("tree"));
    let named: Vec<&str> = lines
        .iter()
        .filter(|line| line["event"] == "file")
        .map(|line| line["path"].as_str().unwrap())
        .collect();
    for file in ["a.bin", "nested/b.bin"] {
        let path = out.join("tree").join(file);
        let times = named.iter().filter(|&&named| Path::new(named) == path);
        assert_eq!(times.count(), 1, "{file} in {named:?}");
    }
    assert!(events.contains(&"progress"), "{events:?}");
    assert!(lines.iter().all(|line| line["v"] == 1));
}

#[test]
fn probe_says_what_a_ticket_names_and_who_holds_it() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("tree");
    small_tree(&source);
    let offer_key = tmp.path().join("offer.key");
    let (_offering, ticket) = offer(&offer_key, &source);
    let id = String::from_utf8(mux(&offer_key).arg("id").output().unwrap().stdout).unwrap();
    let id = id.trim();

    let key = tmp.path().join("probe.key");
    let probed = with_ticket(&key, ["probe"], &ticket).output().unwrap();
    let said = String::from_utf8(probed.stdout).unwrap();
    assert!(
        probed.status.success(),
        "{}",
        String::from_utf8_lossy(&probed.stderr)
    );
    let lines: Vec<&str> = said.lines().collect();
    assert_eq!(
        lines[..6],
        [
            "name     tree",
            "kind     folder",
            "size     3000000 bytes (2.86 MiB)",
            "files    3",
            "checked  verified, if it is copied",
            "parts    read several at once",
        ]
    );
    assert_eq!(lines[6], format!("holder   {}  all of it", &id[..8]));

    let probed = with_ticket(&key, ["--json", "probe"], &ticket)
        .output()
        .unwrap();
    let line: serde_json::Value =
        serde_json::from_str(String::from_utf8(probed.stdout).unwrap().trim()).unwrap();
    assert_eq!(line["event"], "probe");
    assert_eq!(
        (&line["name"], &line["kind"]),
        (&"tree".into(), &"folder".into())
    );
    assert_eq!(line["checked"], "verified");
    assert_eq!(
        line["holders"],
        serde_json::json!([{ "node": id, "bytes": 3_000_000 }])
    );
    // Looking copied nothing.
    assert!(!tmp.path().join("tree.mux-part").exists());
}

#[test]
fn a_dry_run_of_a_ticket_says_what_would_be_copied_and_a_new_name_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("tree");
    small_tree(&source);
    let (_offering, ticket) = offer(&tmp.path().join("offer.key"), &source);
    let key = tmp.path().join("copy.key");
    let out = tmp.path().join("out");
    let into = format!("{}/", out.display());

    let planned = with_ticket(&key, ["cp", "--dry-run"], &ticket)
        .arg(&into)
        .output()
        .unwrap();
    assert!(
        planned.status.success(),
        "{}",
        String::from_utf8_lossy(&planned.stderr)
    );
    assert_result(&planned.stdout, &out.join("tree"), "3000000\tverified");
    assert!(!out.exists(), "a dry run wrote something");

    // What a ticket names keeps its name.
    let renamed = with_ticket(&key, ["cp"], &ticket)
        .arg(tmp.path().join("other-name"))
        .output()
        .unwrap();
    assert!(!renamed.status.success());
    let why = String::from_utf8_lossy(&renamed.stderr).into_owned();
    assert!(why.contains("keeps its own name"), "{why}");
}

#[test]
fn only_what_is_on_this_machine_can_be_offered() {
    let tmp = tempfile::tempdir().unwrap();
    let key = tmp.path().join("node.key");
    let refused = mux(&key)
        .args(["cp", "https://example.org/file.bin", "peer:", "--no-relay"])
        .output()
        .unwrap();
    assert!(!refused.status.success());
    let why = String::from_utf8_lossy(&refused.stderr).into_owned();
    if cfg!(feature = "web") {
        assert!(why.contains("cannot be offered to other machines"), "{why}");
    }
    // Refusing gave this machine no key.
    assert!(!key.exists());
}

#[test]
fn an_offer_is_told_to_a_program_as_a_line_with_its_ticket() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("tree");
    small_tree(&source);
    let mut offering = KillOnDrop(
        mux(&tmp.path().join("offer.key"))
            .args(["--json", "cp"])
            .arg(&source)
            .args(["peer:", "--no-relay", "--listen", "127.0.0.1:0"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut line = String::new();
    BufReader::new(offering.0.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let line: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!((&line["v"], &line["event"]), (&1.into(), &"offer".into()));
    assert_eq!(line["name"], "tree");
    assert_eq!(
        (&line["bytes"], &line["files"]),
        (&3_000_000.into(), &3.into())
    );

    // The ticket in it is one to copy from.
    let out = tmp.path().join("out");
    let copied = with_ticket(
        &tmp.path().join("copy.key"),
        ["cp"],
        line["ticket"].as_str().unwrap(),
    )
    .arg(format!("{}/", out.display()))
    .output()
    .unwrap();
    assert!(
        copied.status.success(),
        "{}",
        String::from_utf8_lossy(&copied.stderr)
    );
    assert_eq!(files_under(&out.join("tree")), files_under(&source));
}
