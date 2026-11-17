//! Nodes that cannot dial each other transfer by way of a relay.

mod common;

use common::{CHUNK, get, index, pattern};
use iroh_relay::server::{RelayConfig, Server, ServerConfig};
use peer::net::{Identity, Options, Relays};
use peer::swarm::Node;
use std::fs;
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

/// A node with no socket of its own. Packets reach it and leave it only by
/// way of the relay at `relay`, which it has reached when this returns.
async fn behind(relay: &str) -> Arc<Node> {
    let options = Options {
        listen: None,
        relays: Relays::At(relay.to_owned()),
        direct: false,
    };
    let node = Node::bind(&Identity::generate(), &options).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), node.online())
        .await
        .expect("the relay was not reached");
    node
}

#[tokio::test]
async fn nodes_that_cannot_dial_each_other_transfer_through_a_relay() {
    let mut config = ServerConfig::default();
    config.relay = Some(RelayConfig::new((Ipv4Addr::LOCALHOST, 0)));
    let relay = Server::spawn(config).await.unwrap();
    let url = format!("http://{}/", relay.http_addr().unwrap());

    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("model.bin");
    let data = pattern(CHUNK * 20 + 7, 5);
    fs::write(&source, &data).unwrap();

    let seeder = behind(&url).await;
    assert_eq!(seeder.local_addrs(), []);
    let (artifact, root) = index(&source);
    seeder.seed(artifact.clone(), root);
    // The ticket says which relay, and no address: the node has none.
    let ticket = seeder.ticket(artifact.id(), &[]);
    let written = format!("mux://{}?peer={}@{url}", artifact.id(), seeder.node_id());
    assert_eq!(ticket.to_string(), written);

    let first = behind(&url).await;
    let out = tmp.path().join("first");
    let report = get(&first, &ticket, &out).await.unwrap();
    assert_eq!(fs::read(out.join("model.bin")).unwrap(), data);
    assert_eq!(report.sources, [(seeder.node_id(), data.len() as u64)]);

    // The seeder goes. A second downloader hears of the first from the
    // ticket's relay alone: its id is all the ticket need say of it.
    seeder.shutdown().await;
    let mut ticket = ticket;
    ticket.peers[0].node = first.node_id();
    let second = behind(&url).await;
    let out = tmp.path().join("second");
    let report = get(&second, &ticket, &out).await.unwrap();
    assert_eq!(fs::read(out.join("model.bin")).unwrap(), data);
    assert_eq!(report.sources, [(first.node_id(), data.len() as u64)]);

    relay.shutdown().await.unwrap();
}
