use p2rent::chunk::{combine_chunks, split_file, Chunk};
use p2rent::crypto::load_or_create_keypair;
use p2rent::manifest::{self, Manifest};
use p2rent::net::quic::{self, QuicClient, QuicServer};
use p2rent::protocol::Message;
use p2rent::storage;
use std::path::PathBuf;

#[tokio::test]
async fn quic_request_chunk_roundtrip() {
    // Prepare sample file and storage
    let temp = tempfile::tempdir().unwrap();
    let storage_dir = temp.path().join("chunks");
    let manifest_dir = temp.path().join("manifests");
    std::fs::create_dir_all(&storage_dir).unwrap();
    std::fs::create_dir_all(&manifest_dir).unwrap();

    let file_path = temp.path().join("hello.txt");
    std::fs::write(&file_path, b"hello world over quic").unwrap();
    let chunks = split_file(&file_path, 8).unwrap();
    let stem = file_path.file_stem().unwrap().to_str().unwrap();
    let file_out_dir = storage_dir.join(stem);
    std::fs::create_dir_all(&file_out_dir).unwrap();
    for c in &chunks {
        storage::save_chunk(file_out_dir.to_str().unwrap(), c).unwrap();
    }
    let manifest = Manifest::from_chunks("hello.txt".into(), 8, &chunks);
    manifest::write_manifest(&manifest, &manifest_dir.join("hello.manifest.json")).unwrap();

    // Start server
    let keypair = load_or_create_keypair().unwrap();
    let addr: std::net::SocketAddr = "127.0.0.1:5600".parse().unwrap();
    let server = QuicServer::bind(addr, keypair.clone()).await.unwrap();

    let serve_dir = storage_dir.clone();
    tokio::spawn(async move {
        loop {
            if let Ok(peer) = server.accept_and_handshake().await {
                let serve_dir = serve_dir.clone();
                tokio::spawn(async move {
                    loop {
                        match peer.connection.accept_bi().await {
                            Ok((mut send, mut recv)) => {
                                if let Ok(Message::RequestChunk { stem, index }) = quic::receive_json_message(&mut recv).await {
                                    let mut dir = serve_dir.clone();
                                    dir.push(stem);
                                    if let Ok(ch) = p2rent::storage::load_chunk(dir.to_str().unwrap(), index) {
                                        let _ = quic::send_json_message(&mut send, &Message::Chunk { index: ch.index, data: ch.data }).await;
                                    }
                                }
                            }
                            Err(_) => break,
                        }
                    }
                });
            }
        }
    });

    // Client fetch a single chunk
    let client = QuicClient::new().await.unwrap();
    let peer = client.connect_and_handshake(addr, &keypair).await.unwrap();
    let (mut send, mut recv) = peer.connection.open_bi().await.unwrap();
    quic::send_json_message(&mut send, &Message::RequestChunk { stem: stem.into(), index: 0 }).await.unwrap();
    if let Message::Chunk { index, data } = quic::receive_json_message(&mut recv).await.unwrap() {
        assert_eq!(index, 0);
        assert_eq!(data.len() > 0, true);
    } else {
        panic!("unexpected response");
    }
}

