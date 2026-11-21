//! Drives the real binary: one process seeds, another downloads.

mod kills;

use kills::{KillOnDrop, Moments};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_mux");

fn mux(key: &Path) -> Command {
    let mut command = Command::new(BIN);
    command.env("MUX_KEY", key).stdin(Stdio::null());
    command
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
    ticket_from(mux(key).args(["seed", "--chunk-size", "64K"]).arg(source))
}

/// Starts an offer on loopback and reads the ticket printed by the command.
fn ticket_from(command: &mut Command) -> (KillOnDrop, String) {
    let mut seeder = KillOnDrop(
        command
            .args(["--no-relay", "--listen", "127.0.0.1:0"])
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

/// What cannot be done as it is typed ends with 2, whichever command it
/// is, and gives this machine no key.
#[test]
fn a_command_that_asks_for_what_cannot_be_done_is_a_usage_error() {
    let tmp = tempfile::tempdir().unwrap();
    let key = tmp.path().join("node.key");
    let file = tmp.path().join("file");
    std::fs::write(&file, b"x").unwrap();
    let file = file.to_str().unwrap();
    let refused: [(&[&str], &str); 4] = [
        (&["seed", file, "--chunk-size", "1"], "chunk size must be"),
        (&["sync", file, "peer:"], "is not a folder on this machine"),
        (&["cp", file, "peer:", "--dry-run"], "--dry-run has nothing"),
        (&["cp", file, "peer:", "--hash", "blake3:00"], ""),
    ];
    for (line, why) in refused {
        let output = mux(&key).args(line).output().unwrap();
        let said = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(2), "{line:?}: {said}");
        assert!(said.contains(why), "{line:?}: {said}");
    }
    assert!(!key.exists());
}

// Another machine through `cp` and `probe`: `peer:` offers, and a ticket is
// a place to copy from.

/// Offers `source` with `mux cp SOURCE peer:` from a new process, and
/// returns it with the ticket it wrote.
fn offer(key: &Path, source: &Path) -> (KillOnDrop, String) {
    ticket_from(mux(key).arg("cp").arg(source).arg("peer:"))
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
fn cp_commits_a_reordered_file_with_no_chunks_fetched() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("sample");
    let chunk = 64 * 1024;
    let new: Vec<_> = (1..=5u8)
        .flat_map(|byte| std::iter::repeat_n(byte, chunk))
        .collect();
    std::fs::write(&path, &new).unwrap();
    let (_seeder, ticket) = seed(&tmp.path().join("sender.key"), &path);
    let out = tmp.path().join("out");
    std::fs::create_dir(&out).unwrap();
    let mut old = new.clone();
    old.rotate_left(chunk);
    std::fs::write(out.join("sample"), old).unwrap();

    let copied = with_ticket(&tmp.path().join("receiver.key"), ["--json", "cp"], &ticket)
        .arg(&out)
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
    assert_eq!(
        lines.iter().filter(|line| line["event"] == "file").count(),
        1
    );
    let done = lines.iter().find(|line| line["event"] == "done").unwrap();
    assert_eq!(done["fetched"], 0);
    assert_eq!(done["kept"], new.len());
    assert_eq!(std::fs::read(out.join("sample")).unwrap(), new);
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
    // done. In between, each file is named once as it gets its name: the
    // one with no bytes too, which needs nothing fetched.
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
    for file in ["a.bin", "empty", "nested/b.bin"] {
        let path = out.join("tree").join(file);
        let times = named.iter().filter(|&&named| Path::new(named) == path);
        assert_eq!(times.count(), 1, "{file} in {named:?}");
    }
    assert_eq!(named.len(), 3, "{named:?}");
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
        lines[..7],
        [
            "name     tree",
            "kind     folder",
            "size     3000000 bytes (2.86 MiB)",
            "files    3",
            "checked  verified, if it is copied",
            "parts    read several at once",
            "swarm    no: it is fetched from the machines the ticket names alone",
        ]
    );
    assert_eq!(lines[7], format!("holder   {}  all of it", &id[..8]));

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
    assert_eq!(line["swarm"], false);
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

    // What a ticket names keeps its name. Nobody is asked: the command is
    // refused as it is typed, and this machine is given no key for it.
    let key = tmp.path().join("unused.key");
    let renamed = with_ticket(&key, ["cp"], &ticket)
        .arg(tmp.path().join("other-name"))
        .output()
        .unwrap();
    let why = String::from_utf8_lossy(&renamed.stderr).into_owned();
    assert_eq!(renamed.status.code(), Some(2), "{why}");
    assert!(why.contains("keeps its own name"), "{why}");
    assert!(!key.exists());
}

#[test]
fn only_what_is_on_this_machine_can_be_offered() {
    let tmp = tempfile::tempdir().unwrap();
    let key = tmp.path().join("node.key");
    let refused = mux(&key)
        .args(["cp", "https://example.org/file.bin", "peer:", "--no-relay"])
        .output()
        .unwrap();
    let why = String::from_utf8_lossy(&refused.stderr).into_owned();
    assert_eq!(refused.status.code(), Some(2), "{why}");
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

/// A second machine that copies what `ticket` names with `--seed`, at a
/// port of its own. Gives the process, where it listens, its node id, and
/// what it writes on standard output after the line for what it saved.
fn copy_and_go_on_offering(tmp: &Path, ticket: &str) -> (KillOnDrop, String, String, impl BufRead) {
    // A port for the second machine, so that a ticket can say where it is.
    let free = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let at = free.local_addr().unwrap().to_string();
    drop(free);
    let key = tmp.join("second.key");
    let id = String::from_utf8(mux(&key).arg("id").output().unwrap().stdout).unwrap();
    let second = tmp.join("second");
    let mut copying = KillOnDrop(
        mux(&key)
            .args(["cp", ticket])
            .arg(format!("{}/", second.display()))
            .args(["--seed", "--no-relay", "--listen", &at])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    // It says what it wrote once that is whole, and does not end.
    let mut said = BufReader::new(copying.0.stdout.take().unwrap());
    let mut line = String::new();
    said.read_line(&mut line).unwrap();
    assert!(line.trim_end().ends_with("\tverified"), "{line:?}");
    (copying, at, id.trim().to_owned(), said)
}

/// `ticket` with the machine `id` at `at` in place of the one it names.
fn naming(ticket: &str, id: &str, at: &str) -> String {
    let (before, after) = ticket.split_once("&peer=").unwrap();
    let rest = after
        .split_once('&')
        .map_or(String::new(), |(_, rest)| format!("&{rest}"));
    format!("{before}&peer={id}@{at}{rest}")
}

/// A machine that copied with `--seed` goes on offering what it copied,
/// under a ticket of its own: with the first offer gone, the next machine
/// copies from it alone. The ticket it was given opens nothing there.
#[test]
fn a_copy_that_goes_on_offering_serves_the_next_machine() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("tree");
    small_tree(&source);
    let (offering, ticket) = offer(&tmp.path().join("offer.key"), &source);

    let (_copying, at, id, mut said) = copy_and_go_on_offering(tmp.path(), &ticket);
    let mut own = String::new();
    said.read_line(&mut own).unwrap();
    let own = own.trim();
    assert!(own.starts_with("mux://"), "{own:?}");
    assert!(own.contains(&id), "{own:?}");
    drop(offering);

    let third = tmp.path().join("third");
    let copied = with_ticket(&tmp.path().join("third.key"), ["cp"], own)
        .arg(format!("{}/", third.display()))
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&copied.stderr).into_owned();
    assert!(copied.status.success(), "{said}");
    assert!(said.contains("from 1 machine"), "{said}");
    assert_eq!(files_under(&third.join("tree")), files_under(&source));

    // The first ticket, pointed at the second machine: its secret is that
    // of the first offer, and the second machine holds nothing under it.
    let passed_on = naming(&ticket, &id, &at);
    let fourth = tmp.path().join("fourth");
    let refused = with_ticket(&tmp.path().join("fourth.key"), ["cp"], &passed_on)
        .arg(format!("{}/", fourth.display()))
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&refused.stderr).into_owned();
    assert_eq!(refused.status.code(), Some(3), "{said}");
    assert!(!fourth.join("tree").exists());
}

/// What a swarm's ticket names is passed on under that ticket: with the
/// first offer gone, the next machine copies from the second with it.
#[test]
fn a_copy_of_what_a_swarms_ticket_names_is_passed_on_under_that_ticket() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("tree");
    small_tree(&source);
    let key = tmp.path().join("offer.key");
    let (offering, ticket) =
        ticket_from(mux(&key).args(["cp", "--swarm"]).arg(&source).arg("peer:"));
    assert!(ticket.ends_with("&swarm=1"), "{ticket:?}");

    let (_copying, at, id, _said) = copy_and_go_on_offering(tmp.path(), &ticket);
    drop(offering);

    let from_the_second = naming(&ticket, &id, &at);
    assert!(from_the_second.ends_with("&swarm=1"), "{from_the_second:?}");
    let third = tmp.path().join("third");
    let copied = with_ticket(&tmp.path().join("third.key"), ["cp"], &from_the_second)
        .arg(format!("{}/", third.display()))
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&copied.stderr).into_owned();
    assert!(copied.status.success(), "{said}");
    assert!(said.contains("from 1 machine"), "{said}");
    assert_eq!(files_under(&third.join("tree")), files_under(&source));
}

/// The last line a command wrote for a program, which says how it failed.
fn failure(stdout: &[u8]) -> serde_json::Value {
    let said = String::from_utf8_lossy(stdout);
    serde_json::from_str(said.lines().last().unwrap()).unwrap()
}

/// What is offered with `--to` is copied by the machines that are named.
/// Another machine that has the ticket is refused, and told what to do.
#[test]
fn an_offer_to_named_machines_refuses_every_other() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("tree");
    small_tree(&source);
    let named = tmp.path().join("named.key");
    let id = String::from_utf8(mux(&named).arg("id").output().unwrap().stdout).unwrap();
    let offering = mux(&tmp.path().join("offer.key"));
    let mut offering = offering;
    let (_offering, ticket) = ticket_from(
        offering
            .args(["cp", "--to", id.trim()])
            .arg(&source)
            .arg("peer:"),
    );

    let out = tmp.path().join("other");
    let refused = with_ticket(&tmp.path().join("other.key"), ["--json", "cp"], &ticket)
        .arg(format!("{}/", out.display()))
        .output()
        .unwrap();
    let why = String::from_utf8_lossy(&refused.stderr).into_owned();
    assert_eq!(refused.status.code(), Some(7), "{why}");
    assert!(why.contains("does not let this machine fetch it"), "{why}");
    assert!(why.contains("hint: ") && why.contains("--to"), "{why}");
    // What is said names the ticket and leaves its secret out.
    assert!(!why.contains("secret="), "{why}");
    let line = failure(&refused.stdout);
    assert_eq!(
        (&line["event"], &line["code"], &line["kind"]),
        (&"error".into(), &7.into(), &"refused".into())
    );
    assert!(!out.exists(), "a copy that was refused wrote something");

    let out = tmp.path().join("named");
    let copied = with_ticket(&named, ["cp"], &ticket)
        .arg(format!("{}/", out.display()))
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&copied.stderr).into_owned();
    assert!(copied.status.success(), "{said}");
    assert_eq!(files_under(&out.join("tree")), files_under(&source));
}

/// What is offered with `--once` is copied by the first machine that asks,
/// as often as it asks, and by no machine after it.
#[test]
fn an_offer_made_once_is_for_the_first_machine_that_asks() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("tree");
    small_tree(&source);
    let mut offering = mux(&tmp.path().join("offer.key"));
    let (_offering, ticket) = ticket_from(offering.args(["seed", "--once"]).arg(&source));

    let copy = |key: &str, out: &str| {
        with_ticket(&tmp.path().join(key), ["cp"], &ticket)
            .arg(format!("{}/", tmp.path().join(out).display()))
            .output()
            .unwrap()
    };
    let first = copy("first.key", "first");
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert_eq!(
        files_under(&tmp.path().join("first/tree")),
        files_under(&source)
    );

    let second = copy("second.key", "second");
    let why = String::from_utf8_lossy(&second.stderr).into_owned();
    assert_eq!(second.status.code(), Some(7), "{why}");
    assert!(!tmp.path().join("second").exists());

    // The first machine is still let in: it finds all of it there.
    let again = copy("first.key", "first");
    let said = String::from_utf8_lossy(&again.stderr).into_owned();
    assert!(again.status.success(), "{said}");
    assert!(said.contains("already there"), "{said}");
}

/// Who may fetch is said of what is offered. A command that offers nothing,
/// or one that passes a swarm on, is refused as it is typed: it opens no
/// socket, and gives this machine no key.
#[test]
fn who_may_fetch_can_be_said_only_of_what_is_offered() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("tree");
    small_tree(&source);
    let key = tmp.path().join("unused.key");
    let out = format!("{}/", tmp.path().join("out").display());
    let id = "ab".repeat(32);
    let of_a_swarm = format!(
        "mux://{id}?secret={}&peer={id}@127.0.0.1:9&swarm=1",
        "5e".repeat(16)
    );
    let source = source.to_str().unwrap();
    let cases: [(&[&str], &str); 5] = [
        (&["cp", source, &out, "--once"], "offers nothing"),
        (&["cp", source, &out, "--swarm"], "offers nothing"),
        (&["cp", source, &out, "--to", &id], "offers nothing"),
        (
            &["cp", &of_a_swarm, &out, "--seed", "--once"],
            "every holder of that ticket",
        ),
        (
            &["cp", source, "peer:", "--swarm", "--to", &id],
            "cannot be used with",
        ),
    ];
    for (typed, why) in cases {
        let refused = mux(&key).args(typed).output().unwrap();
        let said = String::from_utf8_lossy(&refused.stderr).into_owned();
        assert_eq!(refused.status.code(), Some(2), "{typed:?}: {said}");
        assert!(said.contains(why), "{typed:?}: {said}");
    }
    assert!(!key.exists());
    assert!(!tmp.path().join("out").exists());
}

/// Runs a sync against the same live peer, and returns its JSON events.
fn sync_ticket(
    key: &Path,
    ticket: &str,
    to: &Path,
    flags: &[&str],
    exit: i32,
) -> Vec<serde_json::Value> {
    let output = with_ticket(key, ["--json", "sync"], ticket)
        .arg(to)
        .args(flags)
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(exit),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn ticket_sync_plans_then_pulls_only_the_missing_chunks_into_the_named_folder() {
    const CHUNK: usize = 64 * 1024;
    const INSERT: usize = 4096;
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("offered");
    let to = tmp.path().join("renamed");
    std::fs::create_dir_all(source.join("nested")).unwrap();
    std::fs::create_dir_all(source.join("empty-dir")).unwrap();
    let key = tmp.path().join("receiver.key");

    // The selected file starts after unselected chunks in the manifest.
    std::fs::write(source.join("a"), vec![17; CHUNK * 2]).unwrap();
    let old: Vec<_> = (1..=12u8)
        .flat_map(|n| std::iter::repeat_n(n, CHUNK))
        .collect();
    let mut new = old.clone();
    new.splice(CHUNK * 3..CHUNK * 3, std::iter::repeat_n(81, INSERT));
    std::fs::write(source.join("nested/b"), &new).unwrap();
    std::fs::write(source.join("z"), b"same bytes, older timestamp").unwrap();
    let (_seed, ticket) = seed(&tmp.path().join("sender.key"), &source);

    let plan = sync_ticket(&key, &ticket, &to, &["--dry-run"], 0);
    assert_eq!(plan.last().unwrap()["event"], "sync_plan");
    assert!(!to.exists());
    sync_ticket(&key, &ticket, &to, &[], 0);
    assert_eq!(files_under(&to), files_under(&source));
    assert!(to.join("empty-dir").is_dir());

    std::fs::write(to.join("nested/b"), &old).unwrap();
    let old_time = std::time::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
    std::fs::OpenOptions::new()
        .write(true)
        .open(to.join("z"))
        .unwrap()
        .set_modified(old_time)
        .unwrap();
    std::fs::write(to.join("extra"), b"local").unwrap();
    let before = files_under(&to);
    let plan = sync_ticket(&key, &ticket, &to, &["--dry-run", "--delete"], 0);
    let planned = plan.last().unwrap();
    assert_eq!(planned["replace"], 2);
    assert_eq!(planned["remove"], 1);
    assert_eq!(files_under(&to), before);

    let lines = sync_ticket(&key, &ticket, &to, &["--delete"], 0);
    let mut committed: Vec<_> = lines
        .iter()
        .filter(|line| line["event"] == "file")
        .map(|line| Path::new(line["path"].as_str().unwrap()).to_owned())
        .collect();
    committed.sort();
    assert_eq!(committed, [to.join("nested/b"), to.join("z")]);
    let done = lines.last().unwrap();
    assert_eq!(done["event"], "synced");
    assert_eq!(done["fetched"], CHUNK + INSERT);
    assert_eq!(
        done["kept"],
        new.len() - CHUNK - INSERT + b"same bytes, older timestamp".len()
    );
    let trash = Path::new(done["trash"].as_str().unwrap());
    assert_eq!(std::fs::read(trash.join("nested/b")).unwrap(), old);
    assert_eq!(std::fs::read(trash.join("extra")).unwrap(), b"local");
    assert_eq!(
        trash.join("z").metadata().unwrap().modified().unwrap(),
        old_time
    );
    let mut live = files_under(&to);
    live.retain(|name, _| !name.starts_with(".mux-trash/"));
    assert_eq!(live, files_under(&source));
    let again = sync_ticket(&key, &ticket, &to, &["--delete"], 0);
    assert_eq!(again.len(), 1, "{again:?}");
    assert_eq!(again[0]["bytes"], 0);
    assert_eq!(again[0]["fetched"], 0);
}

#[test]
fn ticket_sync_preserves_blocked_paths_and_stops_before_mass_deletion() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    let to = tmp.path().join("out");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("conflict"), b"snapshot").unwrap();
    std::fs::write(source.join("new"), b"from peer").unwrap();
    std::fs::create_dir_all(to.join("conflict")).unwrap();
    for name in ["conflict/mine", "one", "two", "three", "four"] {
        std::fs::write(to.join(name), name).unwrap();
    }
    let (_seed, ticket) = seed(&tmp.path().join("sender.key"), &source);
    let key = tmp.path().join("receiver.key");
    let before = files_under(&to);
    let refused = sync_ticket(&key, &ticket, &to, &["--delete"], 1);
    assert!(
        refused.last().unwrap()["message"]
            .as_str()
            .unwrap()
            .contains("--force")
    );
    assert_eq!(files_under(&to), before);
    assert!(!to.join(".mux-trash").exists());

    let blocked = sync_ticket(&key, &ticket, &to, &[], 1);
    let done = blocked
        .iter()
        .find(|event| event["event"] == "synced")
        .unwrap();
    assert_eq!(done["blocked"], 1);
    assert_eq!(done["fetched"], b"from peer".len());
    assert_eq!(
        std::fs::read(to.join("conflict/mine")).unwrap(),
        b"conflict/mine"
    );
    assert_eq!(std::fs::read(to.join("new")).unwrap(), b"from peer");
    sync_ticket(
        &key,
        &ticket,
        &to,
        &["--delete", "--force", "--no-trash"],
        0,
    );
    assert_eq!(files_under(&to), files_under(&source));
}

#[cfg(unix)]
#[test]
fn ticket_sync_preserves_files_when_the_destination_or_trash_is_a_link() {
    use std::os::unix::fs::symlink;

    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("offered");
    let to = tmp.path().join("chosen");
    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir(&to).unwrap();
    std::fs::write(to.join("mine"), b"local").unwrap();
    let (_seed, ticket) = seed(&tmp.path().join("sender.key"), &source);
    let key = tmp.path().join("receiver.key");
    let alias = tmp.path().join("alias");
    symlink(&to, &alias).unwrap();
    let alias = format!("{}/", alias.display());
    let flags = ["--delete", "--force"];
    sync_ticket(&key, &ticket, Path::new(&alias), &flags, 1);
    assert_eq!(std::fs::read(to.join("mine")).unwrap(), b"local");
    let outside = tmp.path().join("elsewhere");
    std::fs::create_dir(&outside).unwrap();
    symlink(&outside, to.join(".mux-trash")).unwrap();
    sync_ticket(&key, &ticket, &to, &flags, 1);
    assert_eq!(std::fs::read(to.join("mine")).unwrap(), b"local");
    assert_eq!(std::fs::read_dir(outside).unwrap().count(), 0);
}
