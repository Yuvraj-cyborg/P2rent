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

use common::assert_same_tree;
use mux::artifact::Hash;
use server::{Behaviour, Fault, Server};
use std::path::Path;
use std::process::{Command, Output, Stdio};

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
    let output = cp([to, "peer:"]).await;
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
    let then = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_500_000_000);
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
    assert_eq!(
        result(&output),
        [text(&out.join("tree")), &size, "consistent"]
    );

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
