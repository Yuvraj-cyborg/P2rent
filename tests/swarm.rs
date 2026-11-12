//! Several peers: relaying, discovery and dishonest seeders.

mod common;

use common::{CHUNK, get, index, node, options, pattern, peer_info, seed, ticket_for};
use p2rent::Error;
use p2rent::net::{Endpoint, Identity};
use p2rent::swarm::{DownloadOptions, Progress, announce, download};
use std::fs;
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn a_downloader_becomes_a_source() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("model.bin");
    let data = pattern(CHUNK * 30, 1);
    fs::write(&source, &data).unwrap();

    let origin = node();
    let (artifact, ticket) = seed(&origin, &source);
    let first = node();
    get(&first, &ticket, &tmp.path().join("first"))
        .await
        .unwrap();

    // With the origin gone, the first downloader is the only source left.
    origin.shutdown();
    let second = node();
    let relay_ticket = ticket_for(&first, &artifact);
    let report = get(&second, &relay_ticket, &tmp.path().join("second"))
        .await
        .unwrap();

    assert_eq!(fs::read(tmp.path().join("second/model.bin")).unwrap(), data);
    assert_eq!(report.sources, [(first.node_id(), data.len() as u64)]);
    assert_eq!(
        first.share(artifact.id()).unwrap().uploaded_bytes(),
        data.len() as u64
    );
}

#[tokio::test]
async fn status_lists_other_participants() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("model.bin");
    fs::write(&source, pattern(CHUNK * 8, 2)).unwrap();

    let origin = node();
    let (artifact, ticket) = seed(&origin, &source);
    let downloader = node();
    get(&downloader, &ticket, &tmp.path().join("out"))
        .await
        .unwrap();

    let observer = Endpoint::bind(&Identity::generate().unwrap(), common::localhost()).unwrap();
    let connection = observer
        .connect(origin.local_addr().unwrap(), origin.node_id())
        .await
        .unwrap();
    let status = connection.status(artifact.id()).await.unwrap().unwrap();
    assert_eq!(status.peers, [peer_info(&downloader)]);

    // The observer is now a participant too, but is never listed to itself.
    let again = connection.status(artifact.id()).await.unwrap().unwrap();
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
    let liar = node();
    liar.seed(artifact.clone(), forged_dir.join("model.bin"));

    let honest = node();
    honest.seed(artifact.clone(), root);
    let announcer = tokio::spawn(announce(
        honest.clone(),
        artifact.id(),
        vec![peer_info(&liar)],
    ));

    // Wait until the liar knows about the honest node.
    let observer = Endpoint::bind(&Identity::generate().unwrap(), common::localhost()).unwrap();
    let connection = observer
        .connect(liar.local_addr().unwrap(), liar.node_id())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let status = connection.status(artifact.id()).await.unwrap().unwrap();
            if status.peers.contains(&peer_info(&honest)) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the honest node never registered with the liar");

    // The ticket names only the liar.
    let downloader = node();
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
    let liar = node();
    liar.seed(artifact.clone(), forged_dir.join("model.bin"));

    let downloader = node();
    let out = tmp.path().join("out");
    let impatient = DownloadOptions {
        stall_timeout: Duration::from_millis(1500),
        ..options()
    };
    let progress = Arc::new(Progress::default());
    let error = download(
        &downloader,
        &ticket_for(&liar, &artifact),
        &out,
        impatient,
        progress.clone(),
    )
    .await
    .unwrap_err();

    assert!(matches!(error, Error::Stalled(_)), "{error}");
    assert_eq!(progress.fetched_bytes(), 0);
    let written = fs::read(out.join("model.bin")).unwrap();
    assert!(written.iter().all(|&b| b == 0), "forged bytes reached disk");
}

#[tokio::test]
async fn many_downloaders_share_one_origin() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("model.bin");
    let data = pattern(CHUNK * 120, 5);
    fs::write(&source, &data).unwrap();
    let origin = node();
    let (_, ticket) = seed(&origin, &source);

    let mut downloads = tokio::task::JoinSet::new();
    for i in 0..6 {
        let ticket = ticket.clone();
        let out = tmp.path().join(format!("out-{i}"));
        downloads.spawn(async move {
            let downloader = node();
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
