//! Who may fetch: the secret of a ticket, the machines an offer names, and
//! what a machine that fetches gives away of itself.

mod common;

use common::{CHUNK, direct, get, node, pattern, peer_info, seed, seed_for, swarm};
use model::place::Arrived;
use peer::Error;
use peer::net::{Connection, Endpoint, Identity};
use peer::swarm::{Access, Guests, Node, Secret, Ticket};
use std::fs;
use std::path::{Path, PathBuf};

/// A file of a few chunks under `tmp`, and what it holds.
fn source(tmp: &Path) -> (PathBuf, Vec<u8>) {
    let path = tmp.join("model.bin");
    let data = pattern(CHUNK * 4 + 9, 11);
    fs::write(&path, &data).unwrap();
    (path, data)
}

/// A machine that has no node of the library's: it asks what it likes.
/// The endpoint is given back to be kept: one that is let go of closes
/// what it has connected.
async fn asking(node: &Node) -> (Endpoint, Connection) {
    let endpoint = Endpoint::bind(&Identity::generate(), &direct()).await;
    let endpoint = endpoint.unwrap();
    let connection = endpoint.connect(&peer_info(node)).await.unwrap();
    (endpoint, connection)
}

/// Everything a machine can ask of `ticket`'s content over `connection`,
/// showing `secret`: whether each of the three was answered.
async fn answered(connection: &Connection, ticket: &Ticket, secret: &Secret) -> [bool; 3] {
    let found = peer::swarm::find(&node().await, ticket).await.unwrap();
    let content = found.artifact.content_id();
    let len = u32::try_from(CHUNK).unwrap();
    let manifest = connection.manifest(ticket.artifact, secret).await;
    let status = connection.status(content, secret, true).await;
    let arrived = Arrived::default();
    let chunk = connection.chunk(content, 0, len, secret, &arrived).await;
    [
        manifest.unwrap().is_some(),
        status.unwrap().is_some(),
        chunk.unwrap().is_some(),
    ]
}

/// The id of what is offered, the node that offers it and where that is
/// are not enough: without the secret a machine is told what it would be
/// told by a node that holds nothing.
#[tokio::test]
async fn a_machine_without_the_secret_gets_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let (path, _) = source(tmp.path());
    let seeder = node().await;
    let (artifact, ticket) = seed(&seeder, &path);

    let guessed = Ticket {
        secret: Secret::from_bytes([0; 16]),
        ..ticket.clone()
    };
    let out = tmp.path().join("out");
    let error = get(&node().await, &guessed, &out).await.unwrap_err();
    assert!(matches!(error, Error::ManifestUnavailable), "{error}");
    assert!(!out.join("model.bin").exists());

    let (_kept, connection) = asking(&seeder).await;
    assert_eq!(
        answered(&connection, &ticket, &guessed.secret).await,
        [false; 3]
    );
    let share = seeder.share(artifact.content_id()).unwrap();
    assert_eq!(share.uploaded_bytes(), 0);
    // The same machine, once it knows the secret, is answered.
    assert_eq!(
        answered(&connection, &ticket, &ticket.secret).await,
        [true; 3]
    );
}

/// A machine that passes a swarm on asks for the secret as the first
/// machine does.
#[tokio::test]
async fn a_machine_of_a_swarm_gives_nothing_without_the_secret_either() {
    let tmp = tempfile::tempdir().unwrap();
    let (path, _) = source(tmp.path());
    let origin = node().await;
    let (_, ticket) = seed_for(&origin, &path, swarm());
    let passing_on = node().await;
    get(&passing_on, &ticket, &tmp.path().join("out"))
        .await
        .unwrap();

    let (_kept, connection) = asking(&passing_on).await;
    let wrong = Secret::from_bytes([0; 16]);
    assert_eq!(answered(&connection, &ticket, &wrong).await, [false; 3]);
    assert_eq!(
        answered(&connection, &ticket, &ticket.secret).await,
        [true; 3]
    );
}

/// What is offered to machines that are named is fetched by those, and a
/// machine that has the ticket and is not named is told that it is not let
/// in.
#[tokio::test]
async fn only_the_machines_that_are_named_may_fetch() {
    let tmp = tempfile::tempdir().unwrap();
    let (path, data) = source(tmp.path());
    let (named, other) = (node().await, node().await);
    let access = Access {
        guests: Guests::only(vec![named.node_id()]),
        ..Access::new()
    };
    let seeder = node().await;
    let (artifact, ticket) = seed_for(&seeder, &path, access);

    let refused = get(&other, &ticket, &tmp.path().join("other")).await;
    assert!(matches!(refused, Err(Error::Refused)), "{refused:?}");
    assert!(!tmp.path().join("other/model.bin").exists());

    let out = tmp.path().join("named");
    let report = get(&named, &ticket, &out).await.unwrap();
    assert_eq!(fs::read(out.join("model.bin")).unwrap(), data);
    assert_eq!(report.sources, [(seeder.node_id(), data.len() as u64)]);

    // Whatever a machine that is not named asks for, it is told the same.
    let (_kept, connection) = asking(&seeder).await;
    let content = artifact.content_id();
    let status = connection.status(content, &ticket.secret, false).await;
    assert!(matches!(status, Err(Error::Refused)), "{status:?}");
    let len = u32::try_from(CHUNK).unwrap();
    let arrived = Arrived::default();
    let chunk = connection.chunk(content, 0, len, &ticket.secret, &arrived);
    assert!(matches!(chunk.await, Err(Error::Refused)));
    assert_eq!(arrived.so_far(), 0);
    // And without the secret it is not told even that.
    let wrong = Secret::from_bytes([0; 16]);
    let status = connection.status(content, &wrong, false).await;
    assert!(status.unwrap().is_none());
}

/// What is offered to the first machine that asks is fetched by that
/// machine, as often as it asks, and by no machine that comes after it.
#[tokio::test]
async fn the_first_machine_to_ask_is_the_only_one_that_may_fetch() {
    let tmp = tempfile::tempdir().unwrap();
    let (path, data) = source(tmp.path());
    let access = Access {
        guests: Guests::first(1),
        ..Access::new()
    };
    let seeder = node().await;
    let (_, ticket) = seed_for(&seeder, &path, access);

    // A machine that does not know the secret takes nobody's place.
    let guessed = Ticket {
        secret: Secret::from_bytes([0; 16]),
        ..ticket.clone()
    };
    let unknown = get(&node().await, &guessed, &tmp.path().join("unknown")).await;
    assert!(matches!(unknown, Err(Error::ManifestUnavailable)));

    let first = node().await;
    let out = tmp.path().join("first");
    let report = get(&first, &ticket, &out).await.unwrap();
    assert_eq!(report.fetched_bytes, data.len() as u64);

    let second = get(&node().await, &ticket, &tmp.path().join("second")).await;
    assert!(matches!(second, Err(Error::Refused)), "{second:?}");

    // The first carries on with a copy that was cut short.
    fs::write(out.join("model.bin"), &data[..CHUNK * 2]).unwrap();
    let again = get(&first, &ticket, &out).await.unwrap();
    assert_eq!(again.resumed_bytes, (CHUNK * 2) as u64);
    assert_eq!(fs::read(out.join("model.bin")).unwrap(), data);
}

/// Unless the ticket is a swarm's, a machine that fetches is told of to
/// nobody, and answers nobody: not even a machine that has the ticket.
#[tokio::test]
async fn a_machine_that_fetches_is_not_listed_and_serves_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let (path, data) = source(tmp.path());
    let origin = node().await;
    let (artifact, ticket) = seed(&origin, &path);
    let fetching = node().await;
    let out = tmp.path().join("out");
    let report = get(&fetching, &ticket, &out).await.unwrap();
    assert_eq!(fs::read(out.join("model.bin")).unwrap(), data);
    assert!(
        report
            .peers
            .iter()
            .all(|peer| peer.node == origin.node_id())
    );

    // The machine that offers it lists nobody, to one that asks to be
    // listed either: neither the machine that fetched, nor the one asking.
    let content = artifact.content_id();
    let (_kept, at_origin) = asking(&origin).await;
    for _ in 0..2 {
        let status = at_origin.status(content, &ticket.secret, true).await;
        assert_eq!(status.unwrap().unwrap().peers, []);
    }

    // The machine that fetched holds no share, and answers nothing.
    assert!(fetching.share(content).is_none());
    let (_kept, at_fetching) = asking(&fetching).await;
    assert_eq!(
        answered(&at_fetching, &ticket, &ticket.secret).await,
        [false; 3]
    );
}
