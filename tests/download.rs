//! One seeder, one downloader.

mod common;

use common::{CHUNK, assert_same_tree, get, node, pattern, seed};
use p2rent::Error;
use p2rent::artifact::Hash;
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
        seeder.share(artifact.id()).unwrap().uploaded_bytes(),
        data.len() as u64
    );
}

#[tokio::test]
async fn downloads_a_directory_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("dataset");
    fs::create_dir_all(source.join("train/shard")).unwrap();
    fs::write(source.join("README"), b"hello").unwrap();
    fs::write(source.join("empty"), b"").unwrap();
    fs::write(source.join("train/a.bin"), pattern(CHUNK * 9 + 1, 2)).unwrap();
    fs::write(source.join("train/shard/b.bin"), pattern(CHUNK * 4, 3)).unwrap();
    fs::write(source.join("run.sh"), b"#!/bin/sh\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(source.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    }

    let seeder = node();
    let (_, ticket) = seed(&seeder, &source);
    let out = tmp.path().join("out");
    get(&node(), &ticket, &out).await.unwrap();

    assert_same_tree(&source, &out.join("dataset"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |name: &str| {
            fs::metadata(out.join("dataset").join(name))
                .unwrap()
                .permissions()
                .mode()
        };
        assert_ne!(mode("run.sh") & 0o111, 0, "execute bit was lost");
        assert_eq!(mode("README") & 0o111, 0);
    }
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
async fn repairs_corrupted_data_in_place() {
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
