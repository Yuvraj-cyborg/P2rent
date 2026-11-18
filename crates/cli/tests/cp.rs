//! Drives the real binary: `mux cp` from a web server and from a folder,
//! into a folder.
#![cfg(feature = "web")]

// The web server that the tests of `web` are run against: one file, and
// ways of going wrong. Not every test here uses all of it.
#[allow(dead_code)]
#[path = "../../web/tests/origin/server.rs"]
mod server;

// How the tests of `peer` compare two trees on disk.
#[path = "../../peer/tests/common/mod.rs"]
mod common;
mod kills;

use common::assert_same_tree;
use kills::Moments;
use mux::artifact::Hash;
use mux::origins::Origin;
use mux::swarm::{Node, Report, Ticket};
use server::{Behaviour, Fault, Server};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Arc;

const BIN: &str = env!("CARGO_BIN_EXE_mux");
const DATE: &str = "Tue, 17 Nov 2026 10:00:00 GMT";

/// Bytes that do not repeat: a megabyte and a bit, which is two chunks.
fn file() -> Vec<u8> {
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    (0..1_100_000)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[0]
        })
        .collect()
}

/// A server that does everything a good one does.
fn serving(file: Vec<u8>) -> Behaviour {
    Behaviour {
        file,
        ranges: true,
        etag: Some("\"v1\"".into()),
        last_modified: Some(DATE.into()),
        ..Behaviour::default()
    }
}

/// Runs `mux cp` with `args`, away from the thread that serves.
async fn cp<const N: usize>(args: [&str; N]) -> Output {
    let mut command = Command::new(BIN);
    command.arg("cp").args(args).stdin(Stdio::null());
    tokio::task::spawn_blocking(move || command.output().unwrap())
        .await
        .unwrap()
}

fn said(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The line on standard output: what was written, its size, and how far
/// it was checked.
fn result(output: &Output) -> Vec<String> {
    let line = String::from_utf8_lossy(&output.stdout);
    line.trim().split('\t').map(str::to_owned).collect()
}

fn text(path: &Path) -> &str {
    path.to_str().unwrap()
}

/// Checks the line on standard output. The path in it is the destination
/// as it was typed, with the name after it. On Windows that can mix the two
/// ways of writing a path, so it is compared as a path and not as text.
fn assert_result(output: &Output, path: &Path, size: &str, checked: &str) {
    let told = result(output);
    assert_eq!(Path::new(&told[0]), path);
    assert_eq!(told[1..], [size, checked]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_web_address_is_copied_into_a_folder_or_under_a_name() {
    let file = file();
    let server = Server::start(serving(file.clone())).await;
    let url = server.url("/sample.bin");
    let tmp = tempfile::tempdir().unwrap();

    // Into a folder, under the name the address gives.
    let output = cp([&url, text(tmp.path())]).await;
    assert!(output.status.success(), "{}", said(&output));
    let saved = tmp.path().join("sample.bin");
    assert_eq!(std::fs::read(&saved).unwrap(), file);
    let size = file.len().to_string();
    assert_eq!(result(&output), [text(&saved), &size, "consistent"]);

    // Into a folder that is not there yet, written as one.
    let new = format!("{}/new/", text(tmp.path()));
    let output = cp([&url, &new]).await;
    assert!(output.status.success(), "{}", said(&output));
    assert_eq!(
        std::fs::read(Path::new(&new).join("sample.bin")).unwrap(),
        file
    );

    // Under a name of its own.
    let named = tmp.path().join("model.bin");
    let output = cp([&url, text(&named)]).await;
    assert!(output.status.success(), "{}", said(&output));
    assert_eq!(std::fs::read(&named).unwrap(), file);
    assert_eq!(result(&output), [text(&named), &size, "consistent"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn with_its_digest_a_copy_is_verified_and_with_another_it_is_not_kept() {
    let file = file();
    let server = Server::start(serving(file.clone())).await;
    let url = server.url("/sample.bin");
    let tmp = tempfile::tempdir().unwrap();

    let right = format!("blake3:{}", Hash::of(&file));
    let output = cp([&url, text(tmp.path()), "--hash", &right]).await;
    assert!(output.status.success(), "{}", said(&output));
    assert_eq!(result(&output)[2], "verified");

    let other = tempfile::tempdir().unwrap();
    let wrong = format!("blake3:{}", Hash::of(b"another file"));
    let output = cp([&url, text(other.path()), "--hash", &wrong]).await;
    assert_eq!(output.status.code(), Some(6), "{}", said(&output));
    assert!(said(&output).contains("hint: "), "{}", said(&output));
    assert_eq!(output.stdout, b"");
    assert_eq!(std::fs::read_dir(other.path()).unwrap().count(), 0);

    // A digest that is not written as one is a mistake in the command.
    let output = cp([&url, text(other.path()), "--hash", "md5:abc"]).await;
    assert_eq!(output.status.code(), Some(2), "{}", said(&output));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_copy_that_is_there_is_left_alone() {
    let file = file();
    let server = Server::start(serving(file.clone())).await;
    let url = server.url("/sample.bin");
    let tmp = tempfile::tempdir().unwrap();

    let first = cp([&url, text(tmp.path())]).await;
    assert!(first.status.success(), "{}", said(&first));
    let asked = server.asked().len();

    let again = cp([&url, text(tmp.path())]).await;
    assert!(again.status.success(), "{}", said(&again));
    assert_eq!(result(&again)[2], "already there");
    // The server was asked what it holds, and for none of the file.
    assert_eq!(server.asked().len(), asked + 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_copy_that_was_cut_off_says_to_run_it_again_and_is_then_carried_on_with() {
    let file = file();
    let server = Server::start(serving(file.clone())).await;
    let url = server.url("/sample.bin");
    let tmp = tempfile::tempdir().unwrap();
    // Every answer is cut a little way into its second chunk.
    server.fail(Fault::Cut(1_048_576 + 100), usize::MAX);

    let cut = cp([
        &url,
        text(tmp.path()),
        "--streams",
        "1",
        "--stall-timeout",
        "2",
    ])
    .await;
    assert_eq!(cut.status.code(), Some(4), "{}", said(&cut));
    assert!(
        said(&cut).contains("hint: run the same command again"),
        "{}",
        said(&cut)
    );
    assert!(!tmp.path().join("sample.bin").exists());

    server.heal();
    let again = cp([&url, text(tmp.path())]).await;
    assert!(again.status.success(), "{}", said(&again));
    assert_eq!(std::fs::read(tmp.path().join("sample.bin")).unwrap(), file);
    // The first chunk was kept from the time before.
    assert!(
        said(&again).contains("kept     1.00 MiB"),
        "{}",
        said(&again)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn how_a_copy_fails_is_told_by_how_the_command_ends() {
    let tmp = tempfile::tempdir().unwrap();
    let to = text(tmp.path());

    // Nothing at the address.
    let mut gone = serving(file());
    gone.status = Some(404);
    let server = Server::start(gone).await;
    let output = cp([&server.url("/sample.bin"), to]).await;
    assert_eq!(output.status.code(), Some(3), "{}", said(&output));
    assert!(said(&output).contains("nothing is at that address"));

    // Nobody at the address: a port that was just let go of.
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let nobody = format!("http://{}/file", free.local_addr().unwrap());
    drop(free);
    let output = cp([&nobody, to]).await;
    assert_eq!(output.status.code(), Some(4), "{}", said(&output));

    // Places that cannot be copied between.
    let output = cp([to, "https://example.org/there"]).await;
    assert_eq!(output.status.code(), Some(1), "{}", said(&output));
    assert!(
        said(&output).contains("cannot copy from"),
        "{}",
        said(&output)
    );
    // A place that is not one, and a command with nowhere to copy to.
    let output = cp(["gopher://example.org/file", to]).await;
    assert_eq!(output.status.code(), Some(2), "{}", said(&output));
    let output = cp([to]).await;
    assert_eq!(output.status.code(), Some(2), "{}", said(&output));

    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn several_addresses_for_one_file_are_read_together_if_its_digest_is_known() {
    let file = file();
    let first = Server::start(serving(file.clone())).await;
    let second = Server::start(serving(file.clone())).await;
    let (url, mirror) = (first.url("/sample.bin"), second.url("/sample.bin"));
    // The first sends nothing of the file, so all of it is the second's.
    first.fail(Fault::Cut(0), usize::MAX);
    let tmp = tempfile::tempdir().unwrap();
    let digest = format!("blake3:{}", Hash::of(&file));

    let output = cp([
        &url,
        text(tmp.path()),
        "--mirror",
        &mirror,
        "--hash",
        &digest,
    ])
    .await;
    assert!(output.status.success(), "{}", said(&output));
    assert_eq!(std::fs::read(tmp.path().join("sample.bin")).unwrap(), file);
    assert_eq!(result(&output)[2], "verified");

    // Without a digest nothing tells that the two hold the same file.
    let other = tempfile::tempdir().unwrap();
    let output = cp([&url, text(other.path()), "--mirror", &mirror]).await;
    assert_eq!(output.status.code(), Some(1), "{}", said(&output));
    assert!(
        said(&output).contains("without a digest"),
        "{}",
        said(&output)
    );
    assert_eq!(std::fs::read_dir(other.path()).unwrap().count(), 0);
}

/// A folder with what a copy has to keep: files of several sizes, one of
/// them empty, a folder with nothing in it, and where the system has them,
/// a file that may be run and a link.
fn tree(at: &Path) {
    std::fs::create_dir_all(at.join("sub/deep")).unwrap();
    std::fs::create_dir(at.join("nothing in it")).unwrap();
    std::fs::write(at.join("a.bin"), file()).unwrap();
    std::fs::write(at.join("empty"), b"").unwrap();
    std::fs::write(at.join("sub/b.txt"), b"some text").unwrap();
    std::fs::write(at.join("sub/deep/c"), b"c").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let runs = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(at.join("sub/b.txt"), runs).unwrap();
        std::os::unix::fs::symlink("b.txt", at.join("sub/link")).unwrap();
    }
    // A time well in the past, to be kept.
    let then = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_234_567_890);
    let old = std::fs::File::options().write(true).open(at.join("a.bin"));
    old.unwrap().set_modified(then).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_folder_is_copied_with_everything_in_it() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("tree");
    tree(&source);
    let out = tmp.path().join("out");
    let into = format!("{}/", text(&out));

    // Into a folder, under its own name.
    let output = cp([text(&source), &into]).await;
    assert!(output.status.success(), "{}", said(&output));
    assert_same_tree(&source, &out.join("tree"));
    let size = (file().len() + 9 + 1).to_string();
    assert_result(&output, &out.join("tree"), &size, "consistent");

    // Under another name.
    let renamed = out.join("renamed");
    let output = cp([text(&source), text(&renamed)]).await;
    assert!(output.status.success(), "{}", said(&output));
    assert_same_tree(&source, &renamed);

    // One file of it, and two things at once.
    let files = tmp.path().join("files");
    let (a, sub) = (source.join("a.bin"), source.join("sub"));
    let output = cp([text(&a), text(&sub), &format!("{}/", text(&files))]).await;
    assert!(output.status.success(), "{}", said(&output));
    assert_same_tree(&a, &files.join("a.bin"));
    assert_same_tree(&sub, &files.join("sub"));
    assert_eq!(String::from_utf8_lossy(&output.stdout).lines().count(), 2);
}

/// A program is told of each file once as it gets its name: of one with no
/// bytes too, which needs nothing fetched. Copied again, nothing gets a
/// name, and nothing is told.
#[tokio::test(flavor = "multi_thread")]
async fn every_file_of_a_folder_is_told_to_a_program_once() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("tree");
    tree(&source);
    let out = tmp.path().join("out");
    let into = format!("{}/", text(&out));
    let told = |output: &Output| {
        let lines = String::from_utf8_lossy(&output.stdout).into_owned();
        let lines = lines
            .lines()
            .map(|line| serde_json::from_str(line).unwrap());
        let lines: Vec<serde_json::Value> = lines.collect();
        let mut files: Vec<PathBuf> = lines
            .iter()
            .filter(|line| line["event"] == "file")
            .map(|line| PathBuf::from(line["path"].as_str().unwrap()))
            .collect();
        files.sort();
        (lines[0]["files"].as_u64().unwrap(), files)
    };

    let output = mux(["cp", "--json", text(&source), &into]).await;
    assert!(output.status.success(), "{}", said(&output));
    let copy = out.join("tree");
    let (planned, files) = told(&output);
    let all = ["a.bin", "empty", "sub/b.txt", "sub/deep/c"].map(|file| copy.join(file));
    assert_eq!(files, all);
    assert_eq!(planned, 4);

    let output = mux(["cp", "--json", text(&source), &into]).await;
    assert!(output.status.success(), "{}", said(&output));
    assert_eq!(told(&output), (4, Vec::new()));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_folder_that_cannot_be_copied_says_why() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("tree");
    tree(&source);

    // Into itself.
    let inside = format!("{}/sub/", text(&source));
    let output = cp([text(&source), &inside]).await;
    assert_eq!(output.status.code(), Some(1), "{}", said(&output));
    let why = said(&output);
    assert!(why.contains("cannot be copied into itself"), "{why}");
    assert!(!source.join("sub/tree").exists());

    // Nothing there.
    let missing = tmp.path().join("missing");
    let output = cp([text(&missing), text(tmp.path())]).await;
    assert_eq!(output.status.code(), Some(3), "{}", said(&output));

    // What is said of a source on the web.
    let digest = format!("blake3:{}", Hash::of(b"x"));
    let output = cp([text(&source), text(tmp.path()), "--hash", &digest]).await;
    assert_eq!(output.status.code(), Some(1), "{}", said(&output));
    let why = said(&output);
    assert!(why.contains("for a source on the web"), "{why}");
}

/// Runs `mux` with `args`, away from the thread that serves.
async fn mux<const N: usize>(args: [&str; N]) -> Output {
    let mut command = Command::new(BIN);
    command.args(args).stdin(Stdio::null());
    tokio::task::spawn_blocking(move || command.output().unwrap())
        .await
        .unwrap()
}

/// The lines a command wrote for a program, with what differs from run to
/// run put in words: the folder the test works in, and the server's
/// address if there is a server.
fn lines(output: &Output, folder: &Path, server: Option<&str>) -> Vec<String> {
    let written = String::from_utf8_lossy(&output.stdout);
    let folder = text(folder).replace('\\', "\\\\");
    let named = |line: &str| {
        let line = line.replace(&folder, "<DIR>");
        server.map_or(line.clone(), |server| line.replace(server, "<SERVER>"))
    };
    written.lines().map(named).collect()
}

/// The shape of what `--json` writes is part of the interface. This is a
/// whole copy, line for line.
#[tokio::test(flavor = "multi_thread")]
async fn a_copy_is_told_to_a_program_line_by_line() {
    let file = file();
    let server = Server::start(serving(file.clone())).await;
    let url = server.url("/sample.bin");
    let at = server.url("");
    let tmp = tempfile::tempdir().unwrap();
    let digest = format!("blake3:{}", Hash::of(&file));

    let output = mux(["cp", "--json", &url, text(tmp.path()), "--hash", &digest]).await;
    assert!(output.status.success(), "{}", said(&output));
    let mut told = lines(&output, tmp.path(), Some(&at));
    // How often progress is told depends on how fast the copy goes. The
    // last of it is always told, and says that all of it is there.
    let progress: Vec<String> = told
        .iter()
        .filter(|line| line.contains(r#""event":"progress""#))
        .cloned()
        .collect();
    assert_eq!(
        progress.last().map(String::as_str),
        Some(r#"{"v":1,"event":"progress","bytes":1100000,"of":1100000}"#)
    );
    told.retain(|line| !progress.contains(line));
    let separator = std::path::MAIN_SEPARATOR_STR.replace('\\', "\\\\");
    assert_eq!(
        told,
        [
            r#"{"v":1,"event":"start","source":"<SERVER>/sample.bin","destination":"<DIR>","bytes":1100000,"files":1}"#.to_owned(),
            format!(r#"{{"v":1,"event":"file","path":"<DIR>{separator}sample.bin"}}"#),
            format!(
                r#"{{"v":1,"event":"done","path":"<DIR>{separator}sample.bin","bytes":1100000,"checked":"verified","fetched":1100000,"kept":0}}"#
            ),
        ]
    );
    // Every line is JSON, and says which version of the shape it has.
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let read: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(read["v"], 1, "{line}");
    }

    // A failure is the last line, and the command still ends with its code.
    let mut gone = serving(file);
    gone.status = Some(404);
    let server = Server::start(gone).await;
    let output = mux(["cp", "--json", &server.url("/sample.bin"), text(tmp.path())]).await;
    assert_eq!(output.status.code(), Some(3));
    let told = String::from_utf8_lossy(&output.stdout).into_owned();
    let error: serde_json::Value = serde_json::from_str(told.lines().last().unwrap()).unwrap();
    assert_eq!(error["event"], "error");
    assert_eq!(
        (&error["code"], &error["kind"]),
        (&3.into(), &"absent".into())
    );
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("nothing is at that address")
    );
    assert!(error["hint"].is_string());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dry_run_says_what_would_be_copied_and_copies_nothing() {
    let file = file();
    let server = Server::start(serving(file.clone())).await;
    let url = server.url("/sample.bin");
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("out");
    let saved = out.join("sample.bin");
    let into = format!("{}/", text(&out));
    let size = file.len().to_string();

    let output = mux(["cp", "--dry-run", &url, &into]).await;
    assert!(output.status.success(), "{}", said(&output));
    assert_result(&output, &saved, &size, "consistent");
    assert!(said(&output).contains("would copy"), "{}", said(&output));
    // Nothing was made, and the server was asked only what it holds.
    assert!(!out.exists());
    assert_eq!(server.asked(), [Some("bytes=0-0".to_owned())]);

    // Once it is there, a dry run says so, wherever among the places the
    // flag is written.
    let output = mux(["cp", &url, &into]).await;
    assert!(output.status.success(), "{}", said(&output));
    for line in [
        ["cp", "--dry-run", &url, &into],
        ["cp", &url, "--dry-run", &into],
        ["cp", &url, &into, "--dry-run"],
    ] {
        let output = mux(line).await;
        assert_result(&output, &saved, &size, "already there");
    }

    // A folder, for a program.
    let source = tmp.path().join("tree");
    tree(&source);
    let output = mux(["cp", "--dry-run", "--json", text(&source), &into]).await;
    assert!(output.status.success(), "{}", said(&output));
    let told = lines(&output, tmp.path(), None);
    let separator = std::path::MAIN_SEPARATOR_STR.replace('\\', "\\\\");
    assert_eq!(
        told,
        [format!(
            r#"{{"v":1,"event":"plan","source":"<DIR>{separator}tree","destination":"<DIR>{separator}out/","path":"<DIR>{separator}out/tree","bytes":1100010,"files":4,"checked":"consistent"}}"#
        )]
    );
    assert!(!out.join("tree").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn probe_says_what_is_at_a_place() {
    let file = file();
    let server = Server::start(serving(file.clone())).await;
    let url = server.url("/sample.bin");
    let rows = |output: &Output| -> Vec<(String, String)> {
        let table = String::from_utf8_lossy(&output.stdout).into_owned();
        table
            .lines()
            .filter_map(|line| line.split_once(' '))
            .map(|(key, value)| (key.to_owned(), value.trim().to_owned()))
            .collect()
    };
    let row = |key: &str, value: &str| (key.to_owned(), value.to_owned());

    // A file on the web, with a digest said of it.
    let digest = format!("blake3:{}", Hash::of(&file));
    let output = mux(["probe", &url, "--hash", &digest]).await;
    assert!(output.status.success(), "{}", said(&output));
    assert_eq!(
        rows(&output),
        [
            row("name", "sample.bin"),
            row("kind", "file"),
            row("size", "1100000 bytes (1.05 MiB)"),
            row("files", "1"),
            row("checked", "verified, if it is copied"),
            row("parts", "read several at once"),
            row("digest", &digest),
        ]
    );
    // Nothing was read but the first byte.
    assert_eq!(server.asked(), [Some("bytes=0-0".to_owned())]);

    // A folder, for a program.
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("tree");
    tree(&source);
    let output = mux(["probe", "--json", text(&source)]).await;
    assert!(output.status.success(), "{}", said(&output));
    assert_eq!(
        lines(&output, tmp.path(), None),
        [format!(
            r#"{{"v":1,"event":"probe","place":"<DIR>{}tree","name":"tree","kind":"folder","bytes":1100010,"files":4,"checked":"consistent","parts":true,"digests":[]}}"#,
            std::path::MAIN_SEPARATOR_STR.replace('\\', "\\\\")
        )]
    );

    // Nothing there, and a place that cannot be looked at yet.
    let output = mux([
        "probe",
        &server.url("/sample.bin").replace("sample", "missing"),
    ])
    .await;
    assert!(
        output.status.success(),
        "the test server has one file at every path"
    );
    let output = mux(["probe", text(&tmp.path().join("missing"))]).await;
    assert_eq!(output.status.code(), Some(3), "{}", said(&output));
    let output = mux(["probe", "peer:"]).await;
    assert_eq!(output.status.code(), Some(1), "{}", said(&output));
}

/// The steps a dry run of `sync` wrote: what to do, and the path.
fn steps(output: &Output) -> Vec<(String, String)> {
    let written = String::from_utf8_lossy(&output.stdout).into_owned();
    let step = |line: &str| {
        let (step, path) = line.split_once('\t')?;
        Some((step.to_owned(), path.to_owned()))
    };
    written.lines().filter_map(step).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sync_says_what_it_would_do_to_make_a_folder_match() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("tree");
    tree(&source);
    let mirror = tmp.path().join("mirror");
    let step = |step: &str, path: &str| (step.to_owned(), path.to_owned());
    let dry = |more: &'static [&'static str]| {
        let (source, mirror) = (text(&source).to_owned(), text(&mirror).to_owned());
        async move {
            let mut command = Command::new(BIN);
            command
                .args(["sync", "--dry-run", &source, &mirror])
                .args(more);
            command.stdin(Stdio::null());
            tokio::task::spawn_blocking(move || command.output().unwrap())
                .await
                .unwrap()
        }
    };

    // With nothing there, everything is to be added, the folder first.
    let output = dry(&[]).await;
    assert!(output.status.success(), "{}", said(&output));
    let mut everything = vec![
        step("add", "."),
        step("add", "a.bin"),
        step("add", "empty"),
        step("add", "nothing in it"),
        step("add", "sub"),
        step("add", "sub/b.txt"),
        step("add", "sub/deep"),
        step("add", "sub/deep/c"),
    ];
    if cfg!(unix) {
        everything.push(step("add", "sub/link"));
    }
    assert_eq!(steps(&output), everything);
    assert!(said(&output).contains("to bring"), "{}", said(&output));
    assert!(!mirror.exists(), "a dry run made something");

    // Once it is copied, there is nothing to do.
    let copied = cp([text(&source), text(&mirror)]).await;
    assert!(copied.status.success(), "{}", said(&copied));
    let output = dry(&[]).await;
    assert_eq!(steps(&output), []);
    assert!(said(&output).contains("nothing to do"), "{}", said(&output));

    // The source changes: a file grows and one is new. The mirror gets
    // something of its own.
    std::fs::write(source.join("sub/b.txt"), b"some more text").unwrap();
    std::fs::write(source.join("new"), b"new").unwrap();
    std::fs::write(mirror.join("its own"), b"kept").unwrap();
    let wanted = [step("add", "new"), step("replace", "sub/b.txt")];
    let found = |output: &Output| -> Vec<(String, String)> {
        // Changing what a folder holds changes the folder's own time,
        // which a sync would set right. That is not what this test is
        // about.
        let touched = |(step, _): &(String, String)| step == "touch";
        steps(output).into_iter().filter(|s| !touched(s)).collect()
    };
    // What is the mirror's own is left alone unless it is said to go.
    assert_eq!(found(&dry(&[]).await), wanted);
    // The steps come in the order of their paths.
    let with_removal = dry(&["--delete"]).await;
    let removal = step("remove", "its own");
    assert_eq!(
        found(&with_removal),
        [removal, wanted[0].clone(), wanted[1].clone()]
    );

    // For a program, each step and then the count of each.
    let output = dry(&["--delete", "--json"]).await;
    let told: Vec<serde_json::Value> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let (plan, each) = told.split_last().unwrap();
    assert!(each.iter().all(|line| line["event"] == "step"));
    let replaced =
        serde_json::json!({"v": 1, "event": "step", "step": "replace", "path": "sub/b.txt"});
    assert!(each.contains(&replaced), "{each:?}");
    assert_eq!(plan["event"], "sync_plan");
    assert_eq!(
        (&plan["add"], &plan["replace"], &plan["remove"]),
        (&1.into(), &1.into(), &1.into())
    );
    assert_eq!(plan["bytes"], 14 + 3);
}

/// Everything under `root` but the trash of a sync, by its path.
fn held(root: &Path) -> std::collections::BTreeMap<std::path::PathBuf, common::Described> {
    let mut all = common::listing(root);
    all.retain(|path, _| !path.starts_with(".mux-trash"));
    all
}

/// The one folder inside the trash of `root`: that of the one sync that
/// put something there.
fn trash_of(root: &Path) -> std::path::PathBuf {
    let mut syncs: Vec<_> = std::fs::read_dir(root.join(".mux-trash"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(syncs.len(), 1, "{syncs:?}");
    syncs.remove(0)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sync_makes_a_folder_match_and_keeps_what_it_takes_away() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("tree");
    tree(&source);
    let mirror = tmp.path().join("mirror");
    let (from, to) = (text(&source), text(&mirror));

    // The first sync brings everything, and the second finds nothing to do.
    let output = mux(["sync", from, to]).await;
    assert!(output.status.success(), "{}", said(&output));
    assert_eq!(held(&mirror), held(&source));
    assert!(
        !mirror.join(".mux-trash").exists(),
        "nothing was taken away"
    );
    let output = mux(["sync", from, to]).await;
    assert!(output.status.success(), "{}", said(&output));
    assert_eq!(steps(&output), []);
    assert!(said(&output).contains("nothing to do"), "{}", said(&output));

    // The source changes, and the mirror gets something of its own. One
    // file of the source keeps what it holds and may no longer be written.
    std::fs::write(source.join("sub/b.txt"), b"some more text").unwrap();
    std::fs::write(source.join("new"), b"new").unwrap();
    std::fs::remove_file(source.join("empty")).unwrap();
    std::fs::write(mirror.join("its own"), b"kept").unwrap();
    let locked = source.join("sub/deep/c");
    let mut permissions = std::fs::metadata(&locked).unwrap().permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&locked, permissions).unwrap();

    // Without leave to remove, what the mirror has and the source has not
    // stays. What was replaced is in the trash, as it was.
    let output = mux(["sync", from, to]).await;
    assert!(output.status.success(), "{}", said(&output));
    // The file that only changed its permissions was not copied again.
    let touched = ("touch".to_owned(), "sub/deep/c".to_owned());
    assert!(steps(&output).contains(&touched), "{:?}", steps(&output));
    let copy = std::fs::metadata(mirror.join("sub/deep/c")).unwrap();
    assert!(copy.permissions().readonly());
    assert_eq!(
        std::fs::read(mirror.join("sub/b.txt")).unwrap(),
        b"some more text"
    );
    assert_eq!(std::fs::read(mirror.join("new")).unwrap(), b"new");
    assert_eq!(std::fs::read(mirror.join("its own")).unwrap(), b"kept");
    assert!(mirror.join("empty").exists());
    let trash = trash_of(&mirror);
    assert_eq!(
        std::fs::read(trash.join("sub/b.txt")).unwrap(),
        b"some text"
    );
    assert!(said(&output).contains("is in "), "{}", said(&output));
    std::fs::remove_dir_all(mirror.join(".mux-trash")).unwrap();

    // With leave, they go: into the trash. And then the two match.
    let output = mux(["sync", from, to, "--delete"]).await;
    assert!(output.status.success(), "{}", said(&output));
    let removed: Vec<_> = steps(&output)
        .into_iter()
        .filter(|(step, _)| step == "remove")
        .map(|(_, path)| path)
        .collect();
    assert_eq!(removed, ["empty", "its own"]);
    let trash = trash_of(&mirror);
    assert_eq!(std::fs::read(trash.join("its own")).unwrap(), b"kept");
    assert_eq!(std::fs::read(trash.join("empty")).unwrap(), b"");
    assert_eq!(held(&mirror), held(&source));
    // The trash is not taken for something to sync, or to remove.
    let output = mux(["sync", from, to, "--delete"]).await;
    assert_eq!(steps(&output), []);
    assert!(trash.join("its own").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sync_stops_before_it_removes_most_of_a_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("tree");
    tree(&source);
    let mirror = tmp.path().join("mirror");
    let (from, to) = (text(&source), text(&mirror));
    assert!(mux(["sync", from, to]).await.status.success());
    let before = held(&mirror);

    // The source is not there any more, as when a disk is not attached:
    // an empty folder stands in its place.
    let empty = tmp.path().join("empty");
    std::fs::create_dir(&empty).unwrap();
    let output = mux(["sync", text(&empty), to, "--delete"]).await;
    assert_eq!(output.status.code(), Some(1), "{}", said(&output));
    assert!(
        said(&output).contains("more than half"),
        "{}",
        said(&output)
    );
    assert!(said(&output).contains("--force"), "{}", said(&output));
    assert_eq!(held(&mirror), before, "something was changed");
    assert!(!mirror.join(".mux-trash").exists());
    // A dry run says that it would stop.
    let output = mux(["sync", text(&empty), to, "--delete", "--dry-run"]).await;
    assert!(output.status.success(), "{}", said(&output));
    assert!(
        said(&output).contains("a sync would stop here"),
        "{}",
        said(&output)
    );

    // Told to go on, it does, and everything is in the trash.
    let output = mux(["sync", text(&empty), to, "--delete", "--force"]).await;
    assert!(output.status.success(), "{}", said(&output));
    assert_eq!(held(&mirror).len(), 1, "only the folder itself is left");
    let trash = trash_of(&mirror);
    assert_eq!(std::fs::read(trash.join("a.bin")).unwrap(), file());
    assert_eq!(std::fs::read(trash.join("sub/deep/c")).unwrap(), b"c");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sync_can_be_told_to_keep_no_trash() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("tree");
    tree(&source);
    let mirror = tmp.path().join("mirror");
    let (from, to) = (text(&source), text(&mirror));
    assert!(mux(["sync", from, to]).await.status.success());

    std::fs::write(source.join("sub/b.txt"), b"some more text").unwrap();
    std::fs::remove_file(source.join("empty")).unwrap();
    let output = mux(["sync", from, to, "--delete", "--no-trash"]).await;
    assert!(output.status.success(), "{}", said(&output));
    assert_eq!(held(&mirror), held(&source));
    assert!(!mirror.join(".mux-trash").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_folder_in_the_way_is_replaced_only_with_leave_to_remove() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("tree");
    let mirror = tmp.path().join("mirror");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::create_dir_all(mirror.join("x")).unwrap();
    std::fs::write(source.join("x"), b"a file").unwrap();
    std::fs::write(source.join("y"), b"another").unwrap();
    std::fs::write(mirror.join("x/inside"), b"would be lost").unwrap();
    let (from, to) = (text(&source), text(&mirror));

    // Without leave, the folder stays, the rest is done, and the command
    // says that the two do not match.
    let output = mux(["sync", from, to]).await;
    assert_eq!(output.status.code(), Some(1), "{}", said(&output));
    assert!(
        said(&output).contains("left as it was"),
        "{}",
        said(&output)
    );
    assert_eq!(
        std::fs::read(mirror.join("x/inside")).unwrap(),
        b"would be lost"
    );
    assert_eq!(std::fs::read(mirror.join("y")).unwrap(), b"another");

    // With leave, the file takes its place, and the folder is in the trash
    // with what was in it.
    let output = mux(["sync", from, to, "--delete"]).await;
    assert!(output.status.success(), "{}", said(&output));
    assert_eq!(std::fs::read(mirror.join("x")).unwrap(), b"a file");
    let trash = trash_of(&mirror);
    assert_eq!(
        std::fs::read(trash.join("x/inside")).unwrap(),
        b"would be lost"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn one_file_is_synced_with_one_file() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("notes.txt");
    let copy = tmp.path().join("kept").join("copy.txt");
    std::fs::create_dir(tmp.path().join("kept")).unwrap();
    std::fs::write(&source, b"first").unwrap();
    let (from, to) = (text(&source), text(&copy));

    let output = mux(["sync", from, to]).await;
    assert!(output.status.success(), "{}", said(&output));
    assert_eq!(std::fs::read(&copy).unwrap(), b"first");
    assert_eq!(steps(&mux(["sync", from, to]).await), []);

    // The file that is replaced is kept beside it: a file has no inside.
    std::fs::write(&source, b"the second").unwrap();
    let output = mux(["sync", from, to]).await;
    assert!(output.status.success(), "{}", said(&output));
    assert_eq!(std::fs::read(&copy).unwrap(), b"the second");
    let trash = trash_of(&tmp.path().join("kept"));
    assert_eq!(std::fs::read(trash.join("copy.txt")).unwrap(), b"first");

    // A file is not synced with a folder, either way round.
    let folder = tmp.path().join("folder");
    std::fs::create_dir(&folder).unwrap();
    for (from, to) in [(from, text(&folder)), (text(&folder), to)] {
        let output = mux(["sync", from, to, "--delete", "--force"]).await;
        assert_eq!(output.status.code(), Some(1), "{}", said(&output));
        let why = said(&output);
        assert!(why.contains("sync a file with a file"), "{why}");
    }
    assert_eq!(std::fs::read(&copy).unwrap(), b"the second");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_on_the_web_is_synced_and_then_left_alone() {
    let first = file();
    let server = Server::start(serving(first.clone())).await;
    let url = server.url("/sample.bin");
    let tmp = tempfile::tempdir().unwrap();
    let copy = tmp.path().join("copy.bin");

    let output = mux(["sync", &url, text(&copy)]).await;
    assert!(output.status.success(), "{}", said(&output));
    assert_eq!(std::fs::read(&copy).unwrap(), first);
    // Asked again, the server says the same size and time, and nothing of
    // the file is fetched.
    let asked = server.asked().len();
    let output = mux(["sync", &url, text(&copy)]).await;
    assert!(output.status.success(), "{}", said(&output));
    assert_eq!(steps(&output), []);
    assert_eq!(server.asked().len(), asked + 1);

    // The file on the server changes, and the copy follows.
    let second: Vec<u8> = first.iter().rev().copied().chain([7]).collect();
    server.change(|serve| {
        serve.file.clone_from(&second);
        serve.etag = Some("\"v2\"".into());
    });
    let output = mux(["sync", &url, text(&copy), "--json"]).await;
    assert!(output.status.success(), "{}", said(&output));
    assert_eq!(std::fs::read(&copy).unwrap(), second);
    let told = String::from_utf8_lossy(&output.stdout).into_owned();
    let last: serde_json::Value = serde_json::from_str(told.lines().last().unwrap()).unwrap();
    assert_eq!(last["event"], "synced");
    assert_eq!(
        (&last["replace"], &last["bytes"]),
        (&1.into(), &second.len().into())
    );
    assert!(last["trash"].is_string(), "{last}");
}

/// A sync that is cut off has changed nothing that it cannot finish. Run
/// again, it looks at the two places as they are and does what is left.
#[tokio::test(flavor = "multi_thread")]
async fn a_sync_that_was_cut_off_is_finished_by_running_it_again() {
    let first = file();
    let server = Server::start(serving(first.clone())).await;
    let url = server.url("/sample.bin");
    let tmp = tempfile::tempdir().unwrap();
    let copy = tmp.path().join("copy.bin");
    assert!(mux(["sync", &url, text(&copy)]).await.status.success());

    // The file on the server changes, and every answer is cut a little
    // way into its second chunk.
    let second: Vec<u8> = first.iter().rev().copied().chain([7]).collect();
    server.change(|serve| {
        serve.file.clone_from(&second);
        serve.etag = Some("\"v2\"".into());
    });
    server.fail(Fault::Cut(1_048_576 + 100), usize::MAX);
    let cut = mux([
        "sync",
        &url,
        text(&copy),
        "--streams",
        "1",
        "--stall-timeout",
        "2",
    ])
    .await;
    assert_eq!(cut.status.code(), Some(4), "{}", said(&cut));
    assert!(
        said(&cut).contains("hint: run the same command again"),
        "{}",
        said(&cut)
    );
    // The copy is still the whole of the first version.
    assert_eq!(std::fs::read(&copy).unwrap(), first);

    server.heal();
    let again = mux(["sync", &url, text(&copy)]).await;
    assert!(again.status.success(), "{}", said(&again));
    assert_eq!(std::fs::read(&copy).unwrap(), second);
}

// A sync that is killed, or loses its source, and is then run again.

/// `len` bytes that do not repeat, and that differ for each `seed`.
fn bytes(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[0]
        })
        .collect()
}

/// Every file under `root` that has its final name, with what it holds:
/// not what a sync put in its trash, and not a file that is still being
/// written.
fn final_names(root: &Path) -> std::collections::BTreeMap<std::path::PathBuf, Vec<u8>> {
    held(root)
        .into_iter()
        .filter(|(path, _)| !path.to_string_lossy().ends_with(".mux-part"))
        .filter_map(|(path, (seen, _, _))| match seen {
            common::Seen::File(contents) => Some((path, contents)),
            common::Seen::Directory | common::Seen::Link(_) => None,
        })
        .collect()
}

/// Starts `mux` with `args` and stops it dead after `after`, as a crash or
/// a power cut would.
async fn killed<const N: usize>(args: [&str; N], after: std::time::Duration) {
    let mut command = Command::new(BIN);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    tokio::task::spawn_blocking(move || kills::kill(&mut command, after))
        .await
        .unwrap();
}

/// An older tree at `old` and a newer one at `new`: a file that stays, one
/// that changes, one that grows, one that goes and one that comes.
fn two_versions(old: &Path, new: &Path) {
    const MIB: usize = 1024 * 1024;
    for (root, version) in [(old, 1usize), (new, 2)] {
        std::fs::create_dir_all(root.join("nested")).unwrap();
        let write = |name: &str, contents: Vec<u8>| {
            std::fs::write(root.join(name), contents).unwrap();
        };
        write("same.bin", bytes(3 * MIB + 17, 10));
        write("changed.bin", bytes(4 * MIB + version, 20 + version as u64));
        write("nested/grown.bin", bytes(2 * MIB * version + 5, 30));
        write("empty", Vec::new());
    }
    std::fs::write(old.join("nested/gone.bin"), bytes(MIB + 3, 40)).unwrap();
    std::fs::write(new.join("nested/come.bin"), bytes(5 * MIB + 1, 50)).unwrap();
}

/// Checks that each file with its final name is whole: what was there
/// before the sync, or what the source has, and never part of one and part
/// of the other.
fn assert_whole_files(
    found: &std::collections::BTreeMap<std::path::PathBuf, Vec<u8>>,
    was: &std::collections::BTreeMap<std::path::PathBuf, Vec<u8>>,
    wanted: &std::collections::BTreeMap<std::path::PathBuf, Vec<u8>>,
    when: &str,
) {
    for (path, contents) in found {
        let whole = was.get(path) == Some(contents) || wanted.get(path) == Some(contents);
        assert!(
            whole,
            "{when}: {} is neither what was there nor what the source has",
            path.display()
        );
    }
}

/// Stops a sync of a folder dead at random moments, twice over, and then
/// lets one finish. Whenever it stops, every file with its final name is
/// whole. The one that finishes leaves what the source holds.
#[tokio::test(flavor = "multi_thread")]
async fn a_sync_of_a_folder_killed_at_any_moment_leaves_whole_files_and_can_be_finished() {
    const ROUNDS: usize = 6;
    let tmp = tempfile::tempdir().unwrap();
    let (old, new) = (tmp.path().join("old"), tmp.path().join("new"));
    two_versions(&old, &new);
    let (was, wanted) = (final_names(&old), final_names(&new));
    // A destination that holds the older tree.
    let holding_the_old = async |name: &str| {
        let out = tmp.path().join(name);
        let made = mux(["sync", text(&old), text(&out)]).await;
        assert!(made.status.success(), "{}", said(&made));
        out
    };
    let to_the_new = |out: &Path| ["sync", text(&new), text(out), "--delete"].map(str::to_owned);

    // How long an undisturbed sync takes on this machine. The first one
    // pays for what the later ones do not, such as reading the program
    // from disk, so the second is the one timed.
    let mut whole = std::time::Duration::ZERO;
    for name in ["warm", "timed"] {
        let out = holding_the_old(name).await;
        let started = std::time::Instant::now();
        let done = mux(to_the_new(&out).each_ref().map(String::as_str)).await;
        assert!(done.status.success(), "{}", said(&done));
        whole = started.elapsed();
        // Not `assert_eq`, which would print megabytes of both.
        assert!(
            final_names(&out) == wanted,
            "an undisturbed sync left something else"
        );
    }

    let mut moments = Moments::new(ROUNDS, whole);
    while let Some(round) = moments.round() {
        let out = holding_the_old(&format!("out-{round}")).await;
        let args = to_the_new(&out);
        let args = args.each_ref().map(String::as_str);
        // Twice, so that a sync that carries on is cut short too.
        for kill in 0..2 {
            let after = moments.next();
            killed(args, after).await;
            let found = final_names(&out);
            let when = format!("round {round}, killed after {after:?}");
            assert_whole_files(&found, &was, &wanted, &when);
            if kill == 0 {
                moments.first_kill(found != wanted);
            }
        }
        let finished = mux(args).await;
        assert!(
            finished.status.success(),
            "round {round}: {}",
            said(&finished)
        );
        assert!(
            final_names(&out) == wanted,
            "round {round}: the sync that finished left something else"
        );
    }
}

/// The same for a file on the web: the copy is the older version or the
/// newer one whenever the sync stops, and the newer one when it finishes.
#[tokio::test(flavor = "multi_thread")]
async fn a_sync_from_the_web_killed_at_any_moment_leaves_a_whole_file_and_can_be_finished() {
    const ROUNDS: usize = 6;
    const MIB: usize = 1024 * 1024;
    let (first, second) = (bytes(6 * MIB + 11, 1), bytes(7 * MIB + 5, 2));
    let server = Server::start(serving(first.clone())).await;
    let url = server.url("/sample.bin");
    let tmp = tempfile::tempdir().unwrap();
    let serve = |file: &Vec<u8>, etag: &str| {
        server.change(|serve| {
            serve.file.clone_from(file);
            serve.etag = Some(etag.into());
        });
    };
    // A copy of the older version, made while the server has it, and then
    // the server has the newer one.
    let holding_the_first = async |name: &str| {
        serve(&first, "\"v1\"");
        let copy = tmp.path().join(name);
        let made = mux(["sync", &url, text(&copy)]).await;
        assert!(made.status.success(), "{}", said(&made));
        serve(&second, "\"v2\"");
        copy
    };

    let mut whole = std::time::Duration::ZERO;
    for name in ["warm.bin", "timed.bin"] {
        let copy = holding_the_first(name).await;
        let started = std::time::Instant::now();
        let done = mux(["sync", &url, text(&copy)]).await;
        assert!(done.status.success(), "{}", said(&done));
        whole = started.elapsed();
        assert!(
            std::fs::read(&copy).unwrap() == second,
            "an undisturbed sync left something else"
        );
    }

    let mut moments = Moments::new(ROUNDS, whole);
    while let Some(round) = moments.round() {
        let copy = holding_the_first(&format!("copy-{round}.bin")).await;
        for kill in 0..2 {
            let after = moments.next();
            killed(["sync", &url, text(&copy)], after).await;
            let found = std::fs::read(&copy).unwrap();
            assert!(
                found == first || found == second,
                "round {round}, killed after {after:?}: the copy is neither version whole"
            );
            if kill == 0 {
                moments.first_kill(found != second);
            }
        }
        let finished = mux(["sync", &url, text(&copy)]).await;
        assert!(
            finished.status.success(),
            "round {round}: {}",
            said(&finished)
        );
        assert!(
            std::fs::read(&copy).unwrap() == second,
            "round {round}: the sync that finished left something else"
        );
    }
}

/// A source that stops being readable halfway through a sync: the sync
/// fails, every file with its final name is whole, and with the source
/// readable again the same command finishes.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_sync_whose_source_cannot_be_read_harms_nothing_and_can_be_finished() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let (old, new) = (tmp.path().join("old"), tmp.path().join("new"));
    two_versions(&old, &new);
    let (was, wanted) = (final_names(&old), final_names(&new));
    let out = tmp.path().join("out");
    assert!(mux(["sync", text(&old), text(&out)]).await.status.success());

    let locked = new.join("nested/grown.bin");
    let readable = |yes: bool| {
        let mode = if yes { 0o644 } else { 0o000 };
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(mode)).unwrap();
    };
    readable(false);
    if std::fs::read(&locked).is_ok() {
        // Whoever runs this may read anything, so there is nothing to test.
        return;
    }
    let failed = mux(["sync", text(&new), text(&out), "--delete"]).await;
    assert!(!failed.status.success(), "{}", said(&failed));
    assert_whole_files(
        &final_names(&out),
        &was,
        &wanted,
        "with the source unreadable",
    );
    assert_eq!(
        std::fs::read(out.join("nested/grown.bin")).unwrap(),
        was[Path::new("nested/grown.bin")]
    );

    readable(true);
    let finished = mux(["sync", text(&new), text(&out), "--delete"]).await;
    assert!(finished.status.success(), "{}", said(&finished));
    assert_eq!(final_names(&out), wanted);
}

// One transfer fed from the web and from other machines at once.

/// Two machines that hold `file` under the name `sample.bin`, and a ticket
/// that names them both and the web addresses `origins`.
async fn two_holders(
    dir: &Path,
    file: &[u8],
    origins: &[&str],
) -> (Vec<Arc<Node>>, Arc<mux::artifact::Artifact>, Ticket) {
    let held = dir.join("sample.bin");
    std::fs::write(&held, file).unwrap();
    let (artifact, root) = common::index(&held);
    let mut holders = Vec::new();
    let mut ticket = Ticket {
        artifact: artifact.id(),
        peers: Vec::new(),
        origins: origins.iter().map(|&origin| origin.to_owned()).collect(),
    };
    for _ in 0..2 {
        let node = common::node().await;
        node.seed(artifact.clone(), root.clone());
        ticket
            .peers
            .extend(common::ticket_for(&node, &artifact).peers);
        holders.push(node);
    }
    (holders, artifact, ticket)
}

/// Fetches what `ticket` names into `out`, from its machines and its web
/// addresses, and says which addresses were used.
async fn fetch_all(ticket: &Ticket, out: &Path) -> (mux::Result<Report>, Vec<Origin>) {
    let node = common::node().await;
    let mut found = mux::swarm::find(&node, ticket).await.unwrap();
    let origins = mux::origins::add(&mut found, ticket).await;
    let progress = Arc::new(mux::swarm::Progress::default());
    let fetching = mux::swarm::fetch(
        &node,
        found,
        out,
        common::options(),
        progress,
        Arc::new(|_| {}),
    );
    (fetching.await, origins)
}

/// With a slow server and two machines that hold the file, a transfer
/// from all three ends before a copy from the server alone would, and
/// holds the same bytes.
#[tokio::test(flavor = "multi_thread")]
async fn a_slow_server_and_two_machines_are_faster_than_the_server_alone() {
    let file = bytes(2 * 1024 * 1024 + 7, 77);
    // 16 KiB every 25 ms: the file takes over three seconds, however fast
    // the machine the test runs on.
    let pace = std::time::Duration::from_millis(25);
    let mut slow = serving(file.clone());
    slow.pace = Some(pace);
    let server = Server::start(slow).await;
    let url = server.url("/sample.bin");
    let tmp = tempfile::tempdir().unwrap();
    let alone = pace * u32::try_from(file.len() / (16 * 1024)).unwrap();

    let (_holders, _, ticket) = two_holders(tmp.path(), &file, &[&url]).await;
    let started = std::time::Instant::now();
    let out = tmp.path().join("out");
    let (fetched, origins) = fetch_all(&ticket, &out).await;
    let together = started.elapsed();
    let report = fetched.unwrap();

    assert!(std::fs::read(out.join("sample.bin")).unwrap() == file);
    assert_eq!(origins.len(), 1);
    assert_eq!(origins[0].unused, None);
    // About half the time, and it is the server's pace that sets it: its
    // first requests each take a share of the file before anything is
    // known of its speed, and are waited for.
    assert!(
        together < alone,
        "from all three it took {together:?}, and the server alone needs {alone:?}"
    );
    let from_machines: u64 = report.sources.iter().map(|(_, bytes)| bytes).sum();
    assert!(from_machines > 0, "{report:?}");
}

/// A machine that sends other bytes is dropped, and the web address the
/// ticket names fills in for it.
#[tokio::test(flavor = "multi_thread")]
async fn a_web_address_fills_in_for_a_machine_that_sends_wrong_bytes() {
    let file = bytes(1024 * 1024 + 3, 78);
    let server = Server::start(serving(file.clone())).await;
    let url = server.url("/sample.bin");
    let tmp = tempfile::tempdir().unwrap();

    // The manifest of the file, from a machine whose copy holds other
    // bytes of the same length.
    let right = tmp.path().join("right");
    std::fs::create_dir(&right).unwrap();
    std::fs::write(right.join("sample.bin"), &file).unwrap();
    let (artifact, _) = common::index(&right.join("sample.bin"));
    let forged = tmp.path().join("sample.bin");
    std::fs::write(&forged, bytes(file.len(), 79)).unwrap();
    let liar = common::node().await;
    liar.seed(artifact.clone(), forged);
    let mut ticket = common::ticket_for(&liar, &artifact);
    ticket.origins.push(url.clone());

    let out = tmp.path().join("out");
    let (fetched, origins) = fetch_all(&ticket, &out).await;
    let report = fetched.unwrap();
    assert!(std::fs::read(out.join("sample.bin")).unwrap() == file);
    assert_eq!(report.others, [(origins[0].id, file.len() as u64)]);
    assert!(report.sources.iter().all(|(_, bytes)| *bytes == 0));
}

/// A web address that is down, that holds a file of another size, or that
/// holds other bytes is of no use, and the machines finish the transfer.
#[tokio::test(flavor = "multi_thread")]
async fn with_its_web_address_of_no_use_the_machines_finish_the_transfer() {
    let file = bytes(1024 * 1024 + 3, 80);
    let tmp = tempfile::tempdir().unwrap();

    // Nobody at the address: a port that was just let go of.
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let down = format!("http://{}/sample.bin", free.local_addr().unwrap());
    drop(free);
    // A server that holds less, and one that holds as much of other bytes.
    let shorter = Server::start(serving(file[..file.len() - 1].to_vec())).await;
    let other = Server::start(serving(bytes(file.len(), 81))).await;
    let (shorter, other) = (shorter.url("/sample.bin"), other.url("/sample.bin"));

    let origins = [down.as_str(), shorter.as_str(), other.as_str()];
    let (_holders, _, ticket) = two_holders(tmp.path(), &file, &origins).await;
    let out = tmp.path().join("out");
    let (fetched, origins) = fetch_all(&ticket, &out).await;
    let report = fetched.unwrap();

    assert!(std::fs::read(out.join("sample.bin")).unwrap() == file);
    let why: Vec<&str> = origins
        .iter()
        .map(|origin| origin.unused.as_deref().unwrap_or("used"))
        .collect();
    assert!(why[0] != "used", "{why:?}");
    assert!(why[1].contains("bytes"), "{why:?}");
    // The one with other bytes of the right size is found out chunk by
    // chunk: nothing of it is kept.
    assert_eq!(why[2], "used");
    assert!(report.others.iter().all(|(_, bytes)| *bytes == 0));
    let from_machines: u64 = report.sources.iter().map(|(_, bytes)| bytes).sum();
    assert_eq!(from_machines, file.len() as u64);
}
