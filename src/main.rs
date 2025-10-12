use crate::crypto::{generate_keypair, load_or_create_keypair};
use crate::net::quic::{QuicClient, QuicServer};
use std::net::SocketAddr;
use tokio;

#[tokio::main]
async fn main() {
    let keypair = load_or_create_keypair().unwrap();

    let listen_addr: SocketAddr = "127.0.0.1:5000".parse().unwrap();
    let server = QuicServer::bind(listen_addr, keypair).await.unwrap();
    println!("Listening on {}", listen_addr);

    loop {
        match server.accept_and_handshake().await {
            Ok(peer) => {
                println!("Accepted new peer: {}", peer.id);
                tokio::spawn(async move {
                    handle_peer(peer).await;
                });
            }
            Err(e) => {
                eprintln!("Failed to accept peer: {}", e);
            }
        }
    }
}

async fn handle_peer(peer: Peer) {
    // This is where the logic from Step 3 goes.
    // Loop and wait for incoming streams/messages from this peer.
    // e.g., peer.connection.accept_bi().await
    println!("Handling connection with {}", peer.id);
}
