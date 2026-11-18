//! One seeder, one downloader.

mod common;

use common::{CHUNK, assert_same_tree, get, localhost, node, pattern, seed};
use model::artifact::Hash;
use model::place::Arrived;
use peer::Error;
use peer::net::{Endpoint, Identity};
use std::fs;

#[tokio::test]
async fn downloads_a_file() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("model.bin");
    let data = pattern(CHUNK * 75 + 123, 1);
    fs::write(&source, &data).unwrap();

    let seeder = node();
    let (artifact, ticket) = seed(&seeder, &source);
    let downloader = node();
    let out = tmp.path().join("out");
    let report = get(&downloader, &ticket, &out).await.unwrap();

    assert_eq!(fs::read(out.join("model.bin")).unwrap(), data);
    assert_eq!(report.fetched_bytes, data.len() as u64);
    assert_eq!(report.resumed_bytes, 0);
    assert_eq!(report.sources, [(seeder.node_id(), data.len() as u64)]);
    assert!(report.share.is_complete());
    assert_eq!(report.share.artifact().id(), artifact.id());
    assert_eq!(
        seeder
            .share(artifact.content_id())
            .unwrap()
            .uploaded_bytes(),
        data.len() as u64
    );
}

/// A transfer comes through whole with the other way of deciding how fast
/// to send, on either end or on both.
#[tokio::test]
async fn downloads_with_either_congestion_control() {
    use peer::net::Congestion::{Bbr, Cubic};
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("model.bin");
    let data = pattern(CHUNK * 75 + 123, 4);
    fs::write(&source, &data).unwrap();
    let node = |congestion| {
        let identity = Identity::generate().unwrap();
        peer::swarm::Node::bind_with(&identity, localhost(), congestion).unwrap()
    };

    for (number, (sends, fetches)) in [(Bbr, Bbr), (Bbr, Cubic), (Cubic, Bbr)]
        .into_iter()
        .enumerate()
    {
        let seeder = node(sends);
        let (_, ticket) = seed(&seeder, &source);
        let out = tmp.path().join(format!("out-{number}"));
        let report = get(&node(fetches), &ticket, &out).await.unwrap();
        assert_eq!(fs::read(out.join("model.bin")).unwrap(), data);
        assert_eq!(report.fetched_bytes, data.len() as u64);
    }
}

/// Whoever waits for a chunk can see how much of it has come.
#[tokio::test]
async fn the_bytes_of_a_chunk_are_counted_as_they_arrive() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("model.bin");
    let data = pattern(CHUNK * 3 + 7, 4);
    fs::write(&source, &data).unwrap();
    let seeder = node();
    let (artifact, _) = seed(&seeder, &source);

    let asker = Endpoint::bind(&Identity::generate().unwrap(), localhost()).unwrap();
    let connection = asker
        .connect(seeder.local_addr().unwrap(), seeder.node_id())
        .await
        .unwrap();
    let arrived = Arrived::default();
    let len = u32::try_from(CHUNK).unwrap();
    let chunk = connection.chunk(artifact.content_id(), 1, len, &arrived);
    let chunk = chunk.await.unwrap().unwrap();

    assert_eq!(chunk, data[CHUNK..2 * CHUNK]);
    assert_eq!(arrived.so_far(), CHUNK as u64);
}

#[tokio::test]
async fn downloads_a_directory_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("dataset");
    fs::create_dir_all(source.join("train/shard")).unwrap();
    fs::create_dir_all(source.join("hollow/inner")).unwrap();
    fs::write(source.join("README"), b"hello").unwrap();
    fs::write(source.join("empty"), b"").unwrap();
    fs::write(source.join("train/a.bin"), pattern(CHUNK * 9 + 1, 2)).unwrap();
    fs::write(source.join("train/shard/b.bin"), pattern(CHUNK * 4, 3)).unwrap();
    fs::write(source.join("run.sh"), b"#!/bin/sh\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let chmod = |path: &str, mode| {
            fs::set_permissions(source.join(path), fs::Permissions::from_mode(mode)).unwrap();
        };
        chmod("run.sh", 0o755);
        chmod("README", 0o600);
        chmod("train/shard", 0o750);
        std::os::unix::fs::symlink("train/a.bin", source.join("latest")).unwrap();
        std::os::unix::fs::symlink("../README", source.join("train/readme")).unwrap();
        std::os::unix::fs::symlink("gone", source.join("dangling")).unwrap();
    }
    // Times in the past, a different one for each, set from the leaves up:
    // changing what is in a directory changes the directory's time.
    let old = |path: &str, seconds: u64| {
        let time = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(seconds);
        // Windows cannot open a directory like this, nor change a file's
        // time through a handle opened for reading.
        if let Ok(opened) = fs::File::open(source.join(path)) {
            let _ = opened.set_modified(time);
        }
    };
    old("train/shard/b.bin", 1_100_000_000);
    old("train/a.bin", 1_200_000_000);
    old("README", 1_300_000_000);
    old("empty", 1);
    old("train/shard", 1_400_000_000);
    old("train", 1_500_000_000);
    old("hollow/inner", 1_600_000_000);
    old("", 1_700_000_000);

    let seeder = node();
    let (_, ticket) = seed(&seeder, &source);
    let out = tmp.path().join("out");
    let report = get(&node(), &ticket, &out).await.unwrap();

    // Files, empty directories, links, permissions and times all arrive.
    assert_same_tree(&source, &out.join("dataset"));
    assert_eq!(report.skipped_links, 0);
    assert!(out.join("dataset/hollow/inner").is_dir());

    // Fetching it again finds everything in place and changes nothing.
    let again = get(&node(), &ticket, &out).await.unwrap();
    assert_eq!(again.fetched_bytes, 0);
    assert_same_tree(&source, &out.join("dataset"));
}

#[tokio::test]
async fn resumes_from_partial_data() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("model.bin");
    let data = pattern(CHUNK * 40, 4);
    fs::write(&source, &data).unwrap();
    let seeder = node();
    let (_, ticket) = seed(&seeder, &source);

    // The first 25 chunks are intact; the 26th was cut off mid-write.
    let out = tmp.path().join("out");
    fs::create_dir_all(&out).unwrap();
    fs::write(out.join("model.bin"), &data[..CHUNK * 25 + 100]).unwrap();

    let report = get(&node(), &ticket, &out).await.unwrap();
    assert_eq!(report.resumed_bytes, (CHUNK * 25) as u64);
    assert_eq!(report.fetched_bytes, (CHUNK * 15) as u64);
    assert_eq!(fs::read(out.join("model.bin")).unwrap(), data);
}

#[tokio::test]
async fn a_complete_copy_fetches_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("model.bin");
    let data = pattern(CHUNK * 10, 5);
    fs::write(&source, &data).unwrap();
    let seeder = node();
    let (_, ticket) = seed(&seeder, &source);

    let out = tmp.path().join("out");
    fs::create_dir_all(&out).unwrap();
    fs::write(out.join("model.bin"), &data).unwrap();

    let report = get(&node(), &ticket, &out).await.unwrap();
    assert_eq!(report.fetched_bytes, 0);
    assert_eq!(report.resumed_bytes, data.len() as u64);
}

#[tokio::test]
async fn a_damaged_file_is_repaired_by_fetching_only_what_is_wrong() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("model.bin");
    let data = pattern(CHUNK * 10, 6);
    fs::write(&source, &data).unwrap();
    let seeder = node();
    let (_, ticket) = seed(&seeder, &source);

    let mut damaged = data.clone();
    damaged[CHUNK * 3 + 7] ^= 0xff;
    let out = tmp.path().join("out");
    fs::create_dir_all(&out).unwrap();
    fs::write(out.join("model.bin"), &damaged).unwrap();

    let report = get(&node(), &ticket, &out).await.unwrap();
    assert_eq!(report.fetched_bytes, CHUNK as u64);
    assert_eq!(fs::read(out.join("model.bin")).unwrap(), data);
}

#[tokio::test]
async fn an_empty_file_needs_no_chunks() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("empty.bin");
    fs::write(&source, b"").unwrap();
    let seeder = node();
    let (_, ticket) = seed(&seeder, &source);

    let out = tmp.path().join("out");
    let report = get(&node(), &ticket, &out).await.unwrap();
    assert_eq!(report.fetched_bytes, 0);
    assert_eq!(fs::read(out.join("empty.bin")).unwrap(), b"");
}

#[tokio::test]
async fn an_unknown_artifact_is_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let seeder = node();
    let ticket = seeder.ticket(
        Hash::of(b"nobody has this"),
        &[seeder.local_addr().unwrap().to_string()],
    );
    let error = get(&node(), &ticket, tmp.path()).await.unwrap_err();
    assert!(matches!(error, Error::ManifestUnavailable), "{error}");
}

#[tokio::test]
async fn a_ticket_without_peers_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let ticket = node().ticket(Hash::of(b"x"), &[]);
    let error = get(&node(), &ticket, tmp.path()).await.unwrap_err();
    assert!(matches!(error, Error::InvalidTicket(_)), "{error}");
}
