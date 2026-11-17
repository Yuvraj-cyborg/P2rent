//! Several peers: relaying, discovery and dishonest seeders.

mod common;

use common::{CHUNK, get, index, listing, node, options, pattern, peer_info, seed, ticket_for};
use model::artifact::PART_SUFFIX;
use peer::Error;
use peer::net::{Endpoint, Identity};
use peer::swarm::{Progress, announce, download};
use std::fs;
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn a_downloader_becomes_a_source() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("model.bin");
    let data = pattern(CHUNK * 30, 1);
    fs::write(&source, &data).unwrap();

    let origin = node().await;
    let (artifact, ticket) = seed(&origin, &source);
    let first = node().await;
    get(&first, &ticket, &tmp.path().join("first"))
        .await
        .unwrap();

    // With the origin gone, the first downloader is the only source left.
    origin.shutdown().await;
    let second = node().await;
    let relay_ticket = ticket_for(&first, &artifact);
    let report = get(&second, &relay_ticket, &tmp.path().join("second"))
        .await
        .unwrap();

    assert_eq!(fs::read(tmp.path().join("second/model.bin")).unwrap(), data);
    assert_eq!(report.sources, [(first.node_id(), data.len() as u64)]);
    assert_eq!(
        first.share(artifact.content_id()).unwrap().uploaded_bytes(),
        data.len() as u64
    );
}

#[tokio::test]
async fn status_lists_other_participants() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("model.bin");
    fs::write(&source, pattern(CHUNK * 8, 2)).unwrap();

    let origin = node().await;
    let (artifact, ticket) = seed(&origin, &source);
    let downloader = node().await;
    get(&downloader, &ticket, &tmp.path().join("out"))
        .await
        .unwrap();

    let observer = Endpoint::bind(&Identity::generate(), common::localhost())
        .await
        .unwrap();
    let connection = observer
        .connect(origin.local_addr(), origin.node_id())
        .await
        .unwrap();
    let status = connection
        .status(artifact.content_id())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status.peers, [peer_info(&downloader)]);

    // The observer is now a participant too, but is never listed to itself.
    let again = connection
        .status(artifact.content_id())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(again.peers, [peer_info(&downloader)]);
}

#[tokio::test]
async fn a_lying_seeder_is_dropped_and_an_honest_peer_is_found_through_it() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("model.bin");
    let data = pattern(CHUNK * 20, 3);
    fs::write(&source, &data).unwrap();
    let (artifact, root) = index(&source);

    // The liar advertises the real artifact but serves different bytes.
    let forged_dir = tmp.path().join("forged");
    fs::create_dir_all(&forged_dir).unwrap();
    fs::write(forged_dir.join("model.bin"), pattern(CHUNK * 20, 99)).unwrap();
    let liar = node().await;
    liar.seed(artifact.clone(), forged_dir.join("model.bin"));

    let honest = node().await;
    honest.seed(artifact.clone(), root);
    let announcer = tokio::spawn(announce(
        honest.clone(),
        artifact.content_id(),
        vec![peer_info(&liar)],
    ));

    // Wait until the liar knows about the honest node.
    let observer = Endpoint::bind(&Identity::generate(), common::localhost())
        .await
        .unwrap();
    let connection = observer
        .connect(liar.local_addr(), liar.node_id())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let status = connection
                .status(artifact.content_id())
                .await
                .unwrap()
                .unwrap();
            if status.peers.contains(&peer_info(&honest)) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the honest node never registered with the liar");

    // The ticket names only the liar.
    let downloader = node().await;
    let out = tmp.path().join("out");
    let report = get(&downloader, &ticket_for(&liar, &artifact), &out)
        .await
        .unwrap();
    announcer.abort();

    assert_eq!(fs::read(out.join("model.bin")).unwrap(), data);
    assert_eq!(report.sources, [(honest.node_id(), data.len() as u64)]);
    assert!(!report.peers.iter().any(|p| p.node == liar.node_id()));
}

#[tokio::test]
async fn forged_data_is_never_accepted() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("model.bin");
    fs::write(&source, pattern(CHUNK * 6, 4)).unwrap();
    let (artifact, _) = index(&source);

    let forged_dir = tmp.path().join("forged");
    fs::create_dir_all(&forged_dir).unwrap();
    fs::write(forged_dir.join("model.bin"), pattern(CHUNK * 6, 98)).unwrap();
    let liar = node().await;
    liar.seed(artifact.clone(), forged_dir.join("model.bin"));

    let downloader = node().await;
    let out = tmp.path().join("out");
    let progress = Arc::new(Progress::default());
    let error = download(
        &downloader,
        &ticket_for(&liar, &artifact),
        &out,
        options(),
        progress.clone(),
    )
    .await
    .unwrap_err();

    // The only peer was dropped for what it sent, and the download ends
    // there, saying so, and not after waiting for a stall.
    let Error::NoPeerLeft(why) = &error else {
        panic!("{error}");
    };
    assert!(why.contains("failed verification"), "{why}");
    assert_eq!(progress.fetched_bytes(), 0);
    assert!(
        !out.join("model.bin").exists(),
        "an unfinished file has its final name"
    );
    let written = fs::read(out.join(format!("model.bin{PART_SUFFIX}"))).unwrap();
    assert!(written.iter().all(|&b| b == 0), "forged bytes reached disk");
}

#[tokio::test]
async fn many_downloaders_share_one_origin() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("model.bin");
    let data = pattern(CHUNK * 120, 5);
    fs::write(&source, &data).unwrap();
    let origin = node().await;
    let (_, ticket) = seed(&origin, &source);

    let mut downloads = tokio::task::JoinSet::new();
    for i in 0..6 {
        let ticket = ticket.clone();
        let out = tmp.path().join(format!("out-{i}"));
        downloads.spawn(async move {
            let downloader = node().await;
            let report = get(&downloader, &ticket, &out).await.unwrap();
            (out, report)
        });
    }

    let mut total_fetched = 0;
    while let Some(joined) = downloads.join_next().await {
        let (out, report) = joined.unwrap();
        assert_eq!(fs::read(out.join("model.bin")).unwrap(), data);
        total_fetched += report.fetched_bytes;
    }
    assert_eq!(total_fetched, 6 * data.len() as u64);
}

/// Two nodes hold the same bytes under different names, permissions and
/// times. Their manifests differ, so their tickets differ, but the content
/// is one swarm: a download that asked for the first node's artifact is fed
/// by the second.
#[tokio::test]
async fn the_same_bytes_under_other_names_and_times_are_one_swarm() {
    let tmp = tempfile::tempdir().unwrap();
    let tree = |root: &std::path::Path, seconds: u64| {
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("a.bin"), pattern(CHUNK * 12 + 3, 7)).unwrap();
        fs::write(root.join("sub/b.bin"), pattern(CHUNK * 5, 8)).unwrap();
        let time = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(seconds);
        for path in ["a.bin", "sub/b.bin", "sub", ""] {
            // Windows cannot open a directory like this, nor change a
            // file's time through a handle opened for reading.
            if let Ok(opened) = fs::File::open(root.join(path)) {
                let _ = opened.set_modified(time);
            }
        }
    };
    let (first_root, second_root) = (tmp.path().join("one/tree"), tmp.path().join("two/renamed"));
    tree(&first_root, 1_000_000_000);
    tree(&second_root, 1_500_000_000);

    let (first, second) = (node().await, node().await);
    let (wanted, ticket) = seed(&first, &first_root);
    let (other, _) = seed(&second, &second_root);
    assert_ne!(wanted.id(), other.id());
    assert_eq!(wanted.content_id(), other.content_id());

    // The second node holds the content, but not the first node's manifest.
    let observer = Endpoint::bind(&Identity::generate(), common::localhost())
        .await
        .unwrap();
    let connection = observer
        .connect(second.local_addr(), second.node_id())
        .await
        .unwrap();
    assert!(connection.manifest(wanted.id()).await.unwrap().is_none());
    assert!(connection.manifest(other.id()).await.unwrap().is_some());
    assert!(
        connection
            .status(wanted.content_id())
            .await
            .unwrap()
            .is_some()
    );

    // Take the first node's files away. It can still say what the artifact
    // is, and every chunk has to come from the second node.
    let expected = listing(&first_root);
    fs::remove_dir_all(&first_root).unwrap();
    let mut ticket = ticket;
    ticket.peers.extend(ticket_for(&second, &other).peers);

    let out = tmp.path().join("out");
    let report = get(&node().await, &ticket, &out).await.unwrap();
    assert_eq!(report.sources, [(second.node_id(), wanted.total_size())]);
    // What arrives is the artifact that was asked for: the first node's
    // name and times, not the second's.
    assert_eq!(listing(&out.join("tree")), expected);
}
