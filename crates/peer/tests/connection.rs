//! Connections are mutually authenticated by node id.

use peer::Error;
use peer::net::{Endpoint, Identity, NodeId, Options, PeerInfo, Relays};
use std::time::{Duration, Instant};

/// An endpoint reached at one local address and through no relay.
async fn endpoint(identity: &Identity) -> Endpoint {
    let options = Options {
        listen: Some("127.0.0.1:0".parse().unwrap()),
        relays: Relays::Off,
        ..Options::default()
    };
    Endpoint::bind(identity, &options).await.unwrap()
}

/// How to dial `endpoint`, as the node `node`.
fn at(endpoint: &Endpoint, node: NodeId) -> PeerInfo {
    PeerInfo::at(node, endpoint.local_addrs()[0])
}

#[tokio::test]
async fn both_sides_learn_the_other_node_id() {
    let server_identity = Identity::generate();
    let client_identity = Identity::generate();
    let server = endpoint(&server_identity).await;
    let client = endpoint(&client_identity).await;

    let accept = tokio::spawn({
        let server = server.clone();
        async move { server.accept().await.unwrap().establish().await.unwrap() }
    });
    let outbound = client
        .connect(&at(&server, server_identity.node_id()))
        .await
        .unwrap();
    let inbound = accept.await.unwrap();

    assert_eq!(outbound.peer(), server_identity.node_id());
    assert_eq!(inbound.peer(), client_identity.node_id());
    // Each side sees where the other is, and nothing of a relay.
    assert_eq!(inbound.peer_info(), at(&client, client_identity.node_id()));
    assert_eq!(outbound.peer_info(), at(&server, server_identity.node_id()));
}

#[tokio::test]
async fn connecting_to_the_wrong_node_fails() {
    let server = endpoint(&Identity::generate()).await;
    let client = endpoint(&Identity::generate()).await;

    tokio::spawn({
        let server = server.clone();
        async move {
            if let Some(incoming) = server.accept().await {
                let _ = incoming.establish().await;
            }
        }
    });

    let someone_else = Identity::generate().node_id();
    let error = client
        .connect(&at(&server, someone_else))
        .await
        .unwrap_err();
    let Error::Transport(why) = &error else {
        panic!("{error}");
    };
    assert!(why.contains(&someone_else.short()), "{why}");
}

/// A connection starts with datagrams of QUIC's smallest size, and
/// discovery moves it up to what the route and both ends allow. Over
/// loopback the ends are the limit: 1472 bytes, where discovery left to
/// itself stops at 1452.
#[tokio::test]
async fn datagrams_grow_when_the_route_allows() {
    let server_identity = Identity::generate();
    let server = endpoint(&server_identity).await;
    let client = endpoint(&Identity::generate()).await;

    let accept = tokio::spawn({
        let server = server.clone();
        async move { server.accept().await.unwrap().establish().await.unwrap() }
    });
    let outbound = client
        .connect(&at(&server, server_identity.node_id()))
        .await
        .unwrap();
    let inbound = accept.await.unwrap();
    let sizes = || (outbound.path_mtu(), inbound.path_mtu());
    assert!(
        sizes().0 <= Some(1452) && sizes().1 <= Some(1452),
        "connections must start small: {:?}",
        sizes()
    );

    let deadline = Instant::now() + Duration::from_secs(10);
    while sizes().0 <= Some(1452) || sizes().1 <= Some(1452) {
        assert!(
            Instant::now() < deadline,
            "datagrams stayed at {:?}",
            sizes()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
