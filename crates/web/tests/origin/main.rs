//! A file fetched from a web server that behaves, and from one that does
//! not.

mod server;

use base64::Engine;
use engine::conformance::{Sample, check_source};
use engine::{Copied, Error};
use fs::Folder;
use model::artifact::{Entry, EntryKind, EntryMetadata, FileDigest, Hash, MIN_CHUNK_SIZE, Tree};
use model::place::{Failure, Source, Verification};
use server::{Behaviour, Fault, Mistake, Server};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use web::{Options, Origin};

const CHUNK: usize = MIN_CHUNK_SIZE as usize;
const DATE: &str = "Tue, 17 Nov 2026 10:00:00 GMT";
/// The time above, in seconds since 1970.
const DATE_SECONDS: i64 = 1_794_909_600;

/// Bytes that do not repeat.
fn bytes(len: usize) -> Vec<u8> {
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[0]
        })
        .collect()
}

/// Forty chunks and a bit.
fn file() -> Vec<u8> {
    bytes(CHUNK * 40 + 123)
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

/// Short waits, so that a server that has stopped is found out quickly.
fn impatient() -> Options {
    Options {
        connect_timeout: Duration::from_secs(2),
        read_timeout: Duration::from_millis(300),
        digest: None,
    }
}

fn limits(stall_timeout: Duration) -> engine::Options {
    engine::Options {
        stall_timeout,
        ..engine::Options::default()
    }
}

/// Fetches `path` from the server into `dir`.
async fn fetch(server: &Server, path: &str, dir: &Path, options: Options) -> Result<Copied, Error> {
    fetch_within(server, path, dir, options, Duration::from_secs(20)).await
}

async fn fetch_within(
    server: &Server,
    path: &str,
    dir: &Path,
    options: Options,
    stall_timeout: Duration,
) -> Result<Copied, Error> {
    let origin = Origin::find(&server.url(path), &options).await?;
    copy_into(dir, vec![Arc::new(origin)], limits(stall_timeout)).await
}

/// Fetches the file that each of `servers` holds into `dir`.
async fn fetch_from(servers: &[&Server], dir: &Path, options: Options) -> Result<Copied, Error> {
    let mut origins: Vec<Arc<dyn Source>> = Vec::new();
    for server in servers {
        let origin = Origin::find(&server.url("/sample.bin"), &options).await?;
        origins.push(Arc::new(origin));
    }
    copy_into(dir, origins, limits(Duration::from_secs(20))).await
}

async fn copy_into(
    dir: &Path,
    origins: Vec<Arc<dyn Source>>,
    limits: engine::Options,
) -> Result<Copied, Error> {
    let folder = |tree: &Arc<Tree>| Arc::new(Folder::inside(tree.clone(), dir));
    engine::copy(origins, MIN_CHUNK_SIZE, folder, limits, Arc::new(|_| {})).await
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    digest.as_ref().try_into().unwrap()
}

/// The digest of `bytes` as a server gives it in `Repr-Digest`.
fn repr_digest(bytes: &[u8]) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(sha256(bytes));
    format!("sha-256=:{encoded}:")
}

/// The kind of a failure, to compare it by.
fn kind(failure: &Failure) -> &'static str {
    match failure {
        Failure::Unreachable(_) => "unreachable",
        Failure::Absent(_) => "absent",
        Failure::Corrupt(_) => "corrupt",
        Failure::Changed(_) => "changed",
        Failure::Fatal(_) => "fatal",
    }
}

/// The three kinds of server a file can be read from: one that sends parts
/// of a version it names, one that sends only the whole file, and one that
/// names no version but gives the digest of the file.
#[tokio::test]
async fn a_web_address_is_a_source() {
    let file = file();
    let mut whole_only = serving(file.clone());
    whole_only.ranges = false;
    let mut nameless = serving(file.clone());
    nameless.etag = None;
    nameless.last_modified = None;
    nameless.repr_digest = Some(repr_digest(&file));

    for (behaviour, random_access) in [
        (serving(file.clone()), true),
        (whole_only, false),
        (nameless, false),
    ] {
        let server = Server::start(behaviour).await;
        let origin = Origin::find(&server.url("/sample.bin"), &Options::default())
            .await
            .unwrap();
        assert_eq!(origin.capabilities().random_access, random_access);

        // The sample the checks expect: one file, under the address's name.
        let size = file.len() as u64;
        let described = Entry {
            path: String::new(),
            kind: EntryKind::File { size },
        };
        let recorded = EntryMetadata {
            mode: 0o644,
            modified: None,
        };
        let tree = Tree::new("sample.bin", MIN_CHUNK_SIZE, vec![(described, recorded)]).unwrap();
        let digests = file.chunks(CHUNK).map(Hash::of).collect();
        let sample = Sample {
            artifact: Arc::new(tree.with_digests(digests).unwrap()),
            files: vec![file.clone()],
        };
        assert_eq!(check_source(&origin, &sample).await, Ok(()));
    }
}

#[tokio::test]
async fn a_file_arrives_as_it_is_on_the_server() {
    let file = file();
    let server = Server::start(serving(file.clone())).await;
    let tmp = tempfile::tempdir().unwrap();
    let copied = fetch(&server, "/sample.bin", tmp.path(), Options::default())
        .await
        .unwrap();

    let saved = tmp.path().join("sample.bin");
    assert_eq!(std::fs::read(&saved).unwrap(), file);
    assert_eq!(copied.fetched, file.len() as u64);
    assert_eq!(copied.resumed, 0);
    // Nothing vouched for the bytes but the server staying what it was.
    assert_eq!(copied.verification, Some(Verification::Consistent));
    // What was learned is the artifact that the bytes are.
    let learned = copied.artifact.unwrap();
    let digests: Vec<Hash> = file.chunks(CHUNK).map(Hash::of).collect();
    assert_eq!(*learned, copied.tree.with_digests(digests).unwrap());
    // The file has the time the server gave for it.
    let changed = std::fs::metadata(&saved).unwrap().modified().unwrap();
    let since = changed.duration_since(std::time::UNIX_EPOCH).unwrap();
    assert_eq!(since.as_secs(), u64::try_from(DATE_SECONDS).unwrap());
}

/// One request carries a run of chunks: read by one pipeline, the whole
/// file is one request after the one that finds it. With a request for each
/// chunk there would be forty-two.
#[tokio::test]
async fn a_run_of_chunks_is_one_request() {
    let file = file();
    let server = Server::start(serving(file.clone())).await;
    let tmp = tempfile::tempdir().unwrap();
    let origin = Origin::find(&server.url("/sample.bin"), &Options::default())
        .await
        .unwrap();
    let one_at_a_time = engine::Options {
        requests_per_supplier: 1,
        ..engine::Options::default()
    };
    copy_into(tmp.path(), vec![Arc::new(origin)], one_at_a_time)
        .await
        .unwrap();

    assert_eq!(std::fs::read(tmp.path().join("sample.bin")).unwrap(), file);
    let asked = server.asked();
    let asked: Vec<_> = asked.iter().map(|range| range.as_deref()).collect();
    assert_eq!(asked, [Some("bytes=0-0"), Some("bytes=0-")]);
}

#[tokio::test]
async fn a_file_that_is_already_there_is_left_alone() {
    let file = file();
    let server = Server::start(serving(file.clone())).await;
    let tmp = tempfile::tempdir().unwrap();
    fetch(&server, "/sample.bin", tmp.path(), Options::default())
        .await
        .unwrap();
    let before = server.asked().len();

    let again = fetch(&server, "/sample.bin", tmp.path(), Options::default())
        .await
        .unwrap();
    assert_eq!((again.fetched, again.resumed), (0, 0));
    assert_eq!(
        again.verification, None,
        "nothing was checked, and nothing is claimed"
    );
    // The server was asked what it holds, and for nothing more.
    assert_eq!(server.asked().len(), before + 1);

    // A file of another time is fetched again.
    server.change(|serve| serve.last_modified = Some("Wed, 18 Nov 2026 10:00:00 GMT".into()));
    let newer = fetch(&server, "/sample.bin", tmp.path(), Options::default())
        .await
        .unwrap();
    assert_eq!(newer.fetched, file.len() as u64);
}

#[tokio::test]
async fn a_digest_from_the_user_or_the_server_makes_the_file_verified() {
    let file = file();
    let digest = sha256(&file);

    // Given by the user.
    let server = Server::start(serving(file.clone())).await;
    let tmp = tempfile::tempdir().unwrap();
    let given = Options {
        digest: Some(FileDigest::Sha256(digest)),
        ..Options::default()
    };
    let copied = fetch(&server, "/sample.bin", tmp.path(), given)
        .await
        .unwrap();
    assert_eq!(copied.verification, Some(Verification::Verified));

    // Given by the user, as the other kind of digest.
    let tmp = tempfile::tempdir().unwrap();
    let given = Options {
        digest: Some(FileDigest::Blake3(Hash::of(&file))),
        ..Options::default()
    };
    let copied = fetch(&server, "/sample.bin", tmp.path(), given)
        .await
        .unwrap();
    assert_eq!(copied.verification, Some(Verification::Verified));

    // Given by the server, with the file.
    let mut says = serving(file.clone());
    says.repr_digest = Some(repr_digest(&file));
    let server = Server::start(says).await;
    let tmp = tempfile::tempdir().unwrap();
    let copied = fetch(&server, "/sample.bin", tmp.path(), Options::default())
        .await
        .unwrap();
    assert_eq!(copied.verification, Some(Verification::Verified));
    assert_eq!(std::fs::read(tmp.path().join("sample.bin")).unwrap(), file);
}

#[tokio::test]
async fn a_file_without_the_digest_given_for_it_is_not_kept() {
    let server = Server::start(serving(file())).await;
    let tmp = tempfile::tempdir().unwrap();
    let wrong = Options {
        digest: Some(FileDigest::Blake3(Hash::of(b"some other file"))),
        ..Options::default()
    };
    let refused = fetch(&server, "/sample.bin", tmp.path(), wrong)
        .await
        .unwrap_err();

    let Error::Place(Failure::Corrupt(reason)) = &refused else {
        panic!("{refused:?}");
    };
    assert!(
        reason.contains("sample.bin does not have the digest"),
        "{reason}"
    );
    // Neither the file nor anything half written is left.
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn a_connection_that_is_cut_is_carried_on_from() {
    let file = file();
    let server = Server::start(serving(file.clone())).await;
    // Every answer stops a little way into its fourth chunk, once.
    server.fail(Fault::Cut(CHUNK * 3 + 100), 4);
    let tmp = tempfile::tempdir().unwrap();
    let copied = fetch(&server, "/sample.bin", tmp.path(), impatient())
        .await
        .unwrap();

    assert_eq!(std::fs::read(tmp.path().join("sample.bin")).unwrap(), file);
    assert_eq!(copied.fetched, file.len() as u64);
}

#[tokio::test]
async fn a_server_that_stops_sending_is_given_up_on_and_asked_again() {
    let file = file();
    let server = Server::start(serving(file.clone())).await;
    server.fail(Fault::Stall(CHUNK * 2 + 50), 4);
    let tmp = tempfile::tempdir().unwrap();
    let copied = fetch(&server, "/sample.bin", tmp.path(), impatient())
        .await
        .unwrap();

    assert_eq!(std::fs::read(tmp.path().join("sample.bin")).unwrap(), file);
    assert_eq!(copied.fetched, file.len() as u64);
}

/// A transfer that is given up, and run again later, fetches only what it
/// did not get the first time.
#[tokio::test]
async fn a_transfer_that_was_given_up_is_carried_on_when_run_again() {
    let file = file();
    let server = Server::start(serving(file.clone())).await;
    // Every answer is cut, for good, and the transfer waits one second.
    server.fail(Fault::Cut(CHUNK * 3 + 100), usize::MAX);
    let tmp = tempfile::tempdir().unwrap();
    let wait = Duration::from_secs(1);
    let first = fetch_within(&server, "/sample.bin", tmp.path(), impatient(), wait).await;
    assert!(matches!(first, Err(Error::Stalled(_))), "{first:?}");
    assert!(!tmp.path().join("sample.bin").exists());

    server.heal();
    let second = fetch(&server, "/sample.bin", tmp.path(), impatient())
        .await
        .unwrap();
    assert_eq!(std::fs::read(tmp.path().join("sample.bin")).unwrap(), file);
    assert!(
        second.resumed >= 3 * CHUNK as u64,
        "{} bytes kept",
        second.resumed
    );
    assert_eq!(second.fetched + second.resumed, file.len() as u64);
}

#[tokio::test]
async fn a_file_that_changes_on_the_server_ends_the_transfer() {
    let server = Server::start(serving(file())).await;
    let tmp = tempfile::tempdir().unwrap();
    let options = impatient();
    let origin = Origin::find(&server.url("/sample.bin"), &options)
        .await
        .unwrap();
    // The file is replaced after it was looked at and before it is read.
    server.change(|serve| {
        serve.file = bytes(CHUNK * 40 + 123).into_iter().rev().collect();
        serve.etag = Some("\"v2\"".into());
    });
    let within = limits(Duration::from_secs(20));
    let ended = copy_into(tmp.path(), vec![Arc::new(origin)], within)
        .await
        .unwrap_err();

    let Error::Changed(what) = &ended else {
        panic!("{ended:?}");
    };
    assert!(what.contains("has changed on the server"), "{what}");
    assert_eq!(
        std::fs::read_dir(tmp.path()).unwrap().count(),
        0,
        "something was left"
    );
}

/// What an earlier transfer left is of the old version, and is not kept.
#[tokio::test]
async fn what_was_fetched_of_an_older_version_is_fetched_again() {
    let server = Server::start(serving(file())).await;
    server.fail(Fault::Cut(CHUNK * 3 + 100), usize::MAX);
    let tmp = tempfile::tempdir().unwrap();
    let wait = Duration::from_secs(1);
    let first = fetch_within(&server, "/sample.bin", tmp.path(), impatient(), wait).await;
    assert!(matches!(first, Err(Error::Stalled(_))), "{first:?}");

    let newer: Vec<u8> = file().into_iter().rev().collect();
    server.heal();
    server.change(|serve| {
        serve.file.clone_from(&newer);
        serve.etag = Some("\"v2\"".into());
    });
    let second = fetch(&server, "/sample.bin", tmp.path(), impatient())
        .await
        .unwrap();
    assert_eq!(second.resumed, 0);
    assert_eq!(std::fs::read(tmp.path().join("sample.bin")).unwrap(), newer);
}

#[tokio::test]
async fn a_server_with_no_ranges_is_read_as_one_stream() {
    let file = file();
    let mut whole_only = serving(file.clone());
    whole_only.ranges = false;
    let server = Server::start(whole_only).await;
    let origin = Origin::find(&server.url("/sample.bin"), &Options::default())
        .await
        .unwrap();
    assert!(!origin.capabilities().random_access);

    let tmp = tempfile::tempdir().unwrap();
    let copied = fetch(&server, "/sample.bin", tmp.path(), Options::default())
        .await
        .unwrap();
    assert_eq!(std::fs::read(tmp.path().join("sample.bin")).unwrap(), file);
    assert_eq!(copied.verification, Some(Verification::Consistent));
    // Asked what it holds twice, once by this test, and then for the file
    // once, with no part named.
    let asked = server.asked();
    assert_eq!(asked.len(), 3, "{asked:?}");
    assert_eq!(asked[2], None);
}

/// With nothing to tell one version from another, parts fetched apart
/// could be of different versions. One answer is one version.
#[tokio::test]
async fn a_server_that_gives_no_validator_is_read_as_one_stream() {
    let file = file();
    let mut nameless = serving(file.clone());
    nameless.etag = None;
    nameless.last_modified = None;
    let server = Server::start(nameless).await;
    let tmp = tempfile::tempdir().unwrap();
    let copied = fetch(&server, "/sample.bin", tmp.path(), Options::default())
        .await
        .unwrap();

    assert_eq!(std::fs::read(tmp.path().join("sample.bin")).unwrap(), file);
    assert_eq!(copied.fetched, file.len() as u64);
    let asked = server.asked();
    assert_eq!(asked.len(), 2, "{asked:?}");
    assert_eq!(asked[1], None);
    // And with no time given, nothing says the file is already there.
    let again = fetch(&server, "/sample.bin", tmp.path(), Options::default())
        .await
        .unwrap();
    assert_eq!(again.fetched, file.len() as u64);
}

/// A server that sends only the whole file is read again from the start
/// when its answer breaks off, as far as what is still wanted. Its
/// validator says that the second answer is the file the first one was.
#[tokio::test]
async fn a_one_stream_server_that_breaks_off_is_read_again() {
    let file = file();
    let mut whole_only = serving(file.clone());
    whole_only.ranges = false;
    let server = Server::start(whole_only).await;
    server.fail(Fault::Cut(CHUNK * 3 + 100), 1);
    let tmp = tempfile::tempdir().unwrap();
    let copied = fetch(&server, "/sample.bin", tmp.path(), impatient())
        .await
        .unwrap();

    assert_eq!(std::fs::read(tmp.path().join("sample.bin")).unwrap(), file);
    assert_eq!(copied.fetched, file.len() as u64);
}

/// Two answers of a server that names no version could be two files. They
/// are put together only if a digest of the whole file will tell.
#[tokio::test]
async fn answers_are_not_joined_when_nothing_tells_they_are_one_file() {
    let file = file();
    let mut nameless = serving(file.clone());
    nameless.etag = None;
    nameless.last_modified = None;

    let server = Server::start(nameless.clone()).await;
    let origin = Origin::find(&server.url("/sample.bin"), &impatient())
        .await
        .unwrap();
    let refused = origin.open(0, 5).await.unwrap_err();
    assert_eq!(kind(&refused), "fatal", "{refused}");
    assert!(
        refused.to_string().contains("cannot be carried on with"),
        "{refused}"
    );
    // From its start the file can be read, as many times as is wanted.
    let mut reader = origin.open(0, 0).await.unwrap();
    assert_eq!(reader.read(5).await.unwrap(), file[..5]);

    // So an answer that breaks off ends the transfer, which says why, and
    // nothing is kept.
    server.fail(Fault::Cut(CHUNK * 3 + 100), 1);
    let tmp = tempfile::tempdir().unwrap();
    let ended = fetch(&server, "/sample.bin", tmp.path(), impatient()).await;
    let Err(Error::Unsupplied(why)) = &ended else {
        panic!("{ended:?}");
    };
    assert!(why.contains("cannot be carried on with"), "{why}");
    assert!(!tmp.path().join("sample.bin").exists());

    // With a digest of the file, it is carried on, and checked at the end.
    nameless.repr_digest = Some(repr_digest(&file));
    let server = Server::start(nameless).await;
    server.fail(Fault::Cut(CHUNK * 3 + 100), 1);
    let tmp = tempfile::tempdir().unwrap();
    let copied = fetch(&server, "/sample.bin", tmp.path(), impatient())
        .await
        .unwrap();
    assert_eq!(std::fs::read(tmp.path().join("sample.bin")).unwrap(), file);
    assert_eq!(copied.verification, Some(Verification::Verified));
}

/// Several addresses for one file feed one transfer, when a digest of the
/// file tells that they hold the same one.
#[tokio::test]
async fn several_addresses_for_one_file_feed_one_transfer() {
    let file = file();
    let given = Options {
        digest: Some(FileDigest::Sha256(sha256(&file))),
        ..impatient()
    };
    let first = Server::start(serving(file.clone())).await;
    let second = Server::start(serving(file.clone())).await;
    // The first breaks off every answer a little way in, for good, so
    // most of the file has to come from the second.
    first.fail(Fault::Cut(CHUNK * 3 + 100), usize::MAX);
    let tmp = tempfile::tempdir().unwrap();
    let copied = fetch_from(&[&first, &second], tmp.path(), given)
        .await
        .unwrap();

    assert_eq!(std::fs::read(tmp.path().join("sample.bin")).unwrap(), file);
    assert_eq!(copied.verification, Some(Verification::Verified));
    assert_eq!(copied.from.iter().sum::<u64>(), file.len() as u64);
    assert!(copied.from[1] > 0, "{:?}", copied.from);
    assert!(second.asked().len() > 1);
}

#[tokio::test]
async fn addresses_that_cannot_be_known_to_hold_one_file_are_not_read_together() {
    let file = file();
    let given = Options {
        digest: Some(FileDigest::Sha256(sha256(&file))),
        ..impatient()
    };
    let server = Server::start(serving(file.clone())).await;
    let same = Server::start(serving(file.clone())).await;
    let shorter = Server::start(serving(bytes(CHUNK))).await;
    let mut says = serving(file.clone());
    says.repr_digest = Some(repr_digest(&file));
    let says = Server::start(says).await;
    let mut says_another = serving(file.clone());
    says_another.repr_digest = Some(repr_digest(b"another file"));
    let says_another = Server::start(says_another).await;

    for (servers, options, why) in [
        // Nothing tells that the two hold the same file.
        (
            [&server, &same],
            impatient(),
            "sample.bin cannot be read from more than one source",
        ),
        ([&server, &shorter], given, "does not hold the same files"),
        ([&says, &says_another], impatient(), "gives another digest"),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let refused = fetch_from(&servers, tmp.path(), options).await.unwrap_err();
        let Error::Place(Failure::Fatal(said)) = &refused else {
            panic!("{refused:?}");
        };
        assert!(said.contains(why), "{said}");
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
    }
    // Each was asked what it holds, and for nothing more.
    assert_eq!(same.asked(), [Some("bytes=0-0".to_owned())]);
}

/// An address that holds other bytes, at the same length, is found out by
/// the digest, and nothing of the file is kept.
#[tokio::test]
async fn an_address_that_holds_another_file_is_found_out_by_the_digest() {
    let file = file();
    let given = Options {
        digest: Some(FileDigest::Sha256(sha256(&file))),
        ..impatient()
    };
    // The one that holds the file sends nothing of it.
    let honest = Server::start(serving(file.clone())).await;
    honest.fail(Fault::Cut(0), usize::MAX);
    let other = Server::start(serving(file.into_iter().rev().collect())).await;
    let tmp = tempfile::tempdir().unwrap();
    let ended = fetch_from(&[&honest, &other], tmp.path(), given)
        .await
        .unwrap_err();

    assert!(
        matches!(ended, Error::Place(Failure::Corrupt(_))),
        "{ended:?}"
    );
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
}

/// Every answer is held to what the server said when it was first asked.
#[tokio::test]
async fn an_answer_that_is_not_what_was_found_is_not_read() {
    type Change = fn(&mut Behaviour);
    let as_it_is: Change = |_| {};
    let whole_only: Change = |serve| serve.ranges = false;
    let nameless: Change = |serve| {
        serve.etag = None;
        serve.last_modified = None;
    };
    let shorter: Change = |serve| serve.file.truncate(serve.file.len() - 1);
    let cases: [(&str, Change, Change, &str); 6] = [
        (
            "the whole file, where a part of it was asked for",
            as_it_is,
            whole_only,
            "changed",
        ),
        (
            "the whole file, as another version",
            whole_only,
            |serve| serve.etag = Some("\"v2\"".into()),
            "changed",
        ),
        (
            "the whole file, at another length",
            whole_only,
            shorter,
            "changed",
        ),
        (
            "a part, where the whole file was asked for",
            nameless,
            |serve| serve.mistake = Some(Mistake::PartUnasked),
            "corrupt",
        ),
        (
            "a part of a file of another length",
            as_it_is,
            shorter,
            "changed",
        ),
        (
            "a part of another version",
            as_it_is,
            |serve| {
                serve.etag = Some("\"v2\"".into());
                serve.mistake = Some(Mistake::AnyVersion);
            },
            "changed",
        ),
    ];
    for (answer, at_first, then, expected) in cases {
        let mut behaviour = serving(file());
        at_first(&mut behaviour);
        let server = Server::start(behaviour).await;
        let origin = Origin::find(&server.url("/sample.bin"), &impatient())
            .await
            .unwrap();
        server.change(then);
        let refused = origin.open(0, 0).await.unwrap_err();
        assert_eq!(kind(&refused), expected, "{answer}: {refused}");
    }
}

#[tokio::test]
async fn only_the_one_file_is_there_and_only_its_bytes() {
    let file = file();
    let server = Server::start(serving(file.clone())).await;
    let origin = Origin::find(&server.url("/sample.bin"), &impatient())
        .await
        .unwrap();
    let size = file.len() as u64;

    for (entry, offset) in [(1, 0), (0, size), (0, size + 1)] {
        let refused = origin.open(entry, offset).await.unwrap_err();
        assert_eq!(kind(&refused), "absent", "{entry} at {offset}: {refused}");
    }
    // The server was not asked for what could not be there.
    assert_eq!(server.asked().len(), 1);

    let mut last = origin.open(0, size - 1).await.unwrap();
    assert_eq!(last.read(1).await.unwrap(), file[file.len() - 1..]);
}

#[tokio::test]
async fn a_server_that_sends_the_wrong_part_is_not_used() {
    let mut confused = serving(file());
    confused.mistake = Some(Mistake::PartBefore);
    let server = Server::start(confused).await;
    let tmp = tempfile::tempdir().unwrap();
    let ended = fetch(&server, "/sample.bin", tmp.path(), impatient()).await;

    // Nobody else has the file, so the transfer ends, and says why.
    let Err(Error::Unsupplied(why)) = &ended else {
        panic!("{ended:?}");
    };
    assert!(why.contains("was asked for the bytes from"), "{why}");
    assert!(!tmp.path().join("sample.bin").exists());
}

#[tokio::test]
async fn an_empty_file_is_a_file() {
    let server = Server::start(serving(Vec::new())).await;
    let tmp = tempfile::tempdir().unwrap();
    let copied = fetch(&server, "/sample.bin", tmp.path(), Options::default())
        .await
        .unwrap();

    assert_eq!(std::fs::read(tmp.path().join("sample.bin")).unwrap(), b"");
    assert_eq!(copied.fetched, 0);
}

#[tokio::test]
async fn a_redirect_is_followed_and_the_file_keeps_the_name_asked_for() {
    let file = file();
    let server = Server::start(serving(file.clone())).await;
    let tmp = tempfile::tempdir().unwrap();
    fetch(&server, "/moved", tmp.path(), Options::default())
        .await
        .unwrap();

    assert_eq!(std::fs::read(tmp.path().join("moved")).unwrap(), file);
}

#[tokio::test]
async fn an_address_with_no_name_in_it_is_saved_as_download() {
    let file = bytes(CHUNK + 1);
    let server = Server::start(serving(file.clone())).await;
    let tmp = tempfile::tempdir().unwrap();
    fetch(&server, "/", tmp.path(), Options::default())
        .await
        .unwrap();

    assert_eq!(std::fs::read(tmp.path().join("download")).unwrap(), file);
}

#[tokio::test]
async fn what_a_server_refuses_is_said_plainly() {
    for (status, sorted, hint) in [
        (404, "absent", "nothing is at that address"),
        (410, "absent", "nothing is at that address"),
        (401, "fatal", "needs a login"),
        (403, "fatal", "needs a login"),
        (500, "unreachable", "answered 500"),
        (503, "unreachable", "answered 503"),
        (429, "unreachable", "answered 429"),
        (418, "fatal", "answered 418"),
    ] {
        let mut refusing = serving(file());
        refusing.status = Some(status);
        let server = Server::start(refusing).await;
        let refused = Origin::find(&server.url("/sample.bin"), &Options::default())
            .await
            .unwrap_err();
        assert_eq!(kind(&refused), sorted, "{status}: {refused}");
        assert!(refused.to_string().contains(hint), "{status}: {refused}");
    }
}

#[tokio::test]
async fn a_server_that_does_not_say_how_large_the_file_is_cannot_be_used() {
    let mut vague = serving(file());
    vague.ranges = false;
    vague.no_length = true;
    let server = Server::start(vague).await;
    let refused = Origin::find(&server.url("/sample.bin"), &Options::default())
        .await
        .unwrap_err();
    assert!(
        refused.to_string().contains("does not say how large"),
        "{refused}"
    );
}

#[tokio::test]
async fn what_is_not_a_web_address_or_answers_nothing_is_a_failure() {
    // Trying these again would not help.
    let server = Server::start(serving(file())).await;
    for address in [
        "not a url",
        "ftp://example.invalid/file",
        &server.url("/loop"),
    ] {
        let refused = Origin::find(address, &impatient()).await.unwrap_err();
        assert_eq!(kind(&refused), "fatal", "{address}: {refused}");
    }

    // Nothing listens on a port that was just let go of.
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/file", free.local_addr().unwrap());
    drop(free);
    let nobody = Origin::find(&url, &impatient()).await.unwrap_err();
    assert!(matches!(nobody, Failure::Unreachable(_)), "{nobody}");
}

/// A proxy named in the environment is used. The environment belongs to
/// the whole process, so the fetch is made by a child process that has the
/// variable set, running the test after this one and nothing else.
#[tokio::test(flavor = "multi_thread")]
async fn a_proxy_named_in_the_environment_is_used() {
    let file = bytes(CHUNK + 1);
    let server = Server::start(serving(file)).await;
    // The address is one nothing answers at: only a proxy can fetch it.
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "through_the_proxy_of_the_environment",
            "--nocapture",
        ])
        .env("http_proxy", server.url(""))
        .env("MUX_TEST_FETCH", "http://nothing.invalid/sample.bin")
        .output();
    let child = tokio::task::spawn_blocking(move || child)
        .await
        .unwrap()
        .unwrap();
    assert!(
        child.status.success(),
        "{}",
        String::from_utf8_lossy(&child.stdout)
    );
    // The proxy, which is the test server, was asked for the first byte.
    assert_eq!(server.asked(), [Some("bytes=0-0".to_owned())]);
}

/// Run by the test above, in a process of its own. On its own it does
/// nothing.
#[tokio::test]
async fn through_the_proxy_of_the_environment() {
    let Ok(url) = std::env::var("MUX_TEST_FETCH") else {
        return;
    };
    let origin = Origin::find(&url, &impatient()).await.unwrap();
    let described = origin.describe().await.unwrap();
    assert_eq!(described.entries[0].0.size(), CHUNK as u64 + 1);
}
