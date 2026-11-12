//! Connections are mutually authenticated by node id.

use p2rent::Error;
use p2rent::net::{Endpoint, Identity};
use std::net::SocketAddr;

fn localhost() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

#[tokio::test]
async fn both_sides_learn_the_other_node_id() {
    let server_identity = Identity::generate().unwrap();
    let client_identity = Identity::generate().unwrap();
    let server = Endpoint::bind(&server_identity, localhost()).unwrap();
    let client = Endpoint::bind(&client_identity, localhost()).unwrap();
    let addr = server.local_addr().unwrap();

    let accept = tokio::spawn({
        let server = server.clone();
        async move { server.accept().await.unwrap().establish().await.unwrap() }
    });
    let outbound = client
        .connect(addr, server_identity.node_id())
        .await
        .unwrap();
    let inbound = accept.await.unwrap();

    assert_eq!(outbound.peer(), server_identity.node_id());
    assert_eq!(inbound.peer(), client_identity.node_id());
    assert_eq!(inbound.remote_addr(), client.local_addr().unwrap());
}

#[tokio::test]
async fn connecting_to_the_wrong_node_fails() {
    let server = Endpoint::bind(&Identity::generate().unwrap(), localhost()).unwrap();
    let client = Endpoint::bind(&Identity::generate().unwrap(), localhost()).unwrap();
    let addr = server.local_addr().unwrap();

    tokio::spawn({
        let server = server.clone();
        async move {
            if let Some(incoming) = server.accept().await {
                let _ = incoming.establish().await;
            }
        }
    });

    let someone_else = Identity::generate().unwrap().node_id();
    let error = client.connect(addr, someone_else).await.unwrap_err();
    assert!(
        matches!(error, Error::IdentityMismatch { .. }),
        "unexpected error: {error}"
    );
}
