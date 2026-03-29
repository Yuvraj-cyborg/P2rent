use p2rent::chunk::split_file;
use p2rent::crypto::load_or_create_keypair;
use p2rent::manifest::{self, Manifest};
use p2rent::net::protocol::Message;
use p2rent::net::quic::{self, QuicClient, QuicServer};
use p2rent::storage;

#[tokio::test]
async fn quic_request_chunk_roundtrip() {
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

    let keypair = load_or_create_keypair().unwrap();
    let addr: std::net::SocketAddr = "127.0.0.1:5600".parse().unwrap();
    let server = QuicServer::bind(addr, keypair.clone()).await.unwrap();

    let serve_dir = storage_dir.clone();
    tokio::spawn(async move {
        loop {
            if let Ok(peer) = server.accept_and_handshake().await {
                let serve_dir = serve_dir.clone();
                tokio::spawn(async move {
                    while let Ok((mut send, mut recv)) = peer.connection.accept_bi().await {
                        if let Ok(Message::RequestChunk { stem, index }) =
                            quic::receive_message(&mut recv).await
                        {
                            let mut dir = serve_dir.clone();
                            dir.push(stem);
                            if let Ok(ch) = storage::load_chunk(dir.to_str().unwrap(), index) {
                                let _ = quic::send_message(
                                    &mut send,
                                    &Message::Chunk {
                                        index: ch.index,
                                        data: ch.data,
                                    },
                                )
                                .await;
                            }
                        }
                    }
                });
            }
        }
    });

    let client = QuicClient::new().await.unwrap();
    let peer = client.connect_and_handshake(addr, &keypair).await.unwrap();
    let (mut send, mut recv) = peer.connection.open_bi().await.unwrap();
    quic::send_message(
        &mut send,
        &Message::RequestChunk {
            stem: stem.into(),
            index: 0,
        },
    )
    .await
    .unwrap();
    if let Message::Chunk { index, data } = quic::receive_message(&mut recv).await.unwrap() {
        assert_eq!(index, 0);
        assert!(!data.is_empty());
    } else {
        panic!("unexpected response");
    }
}
