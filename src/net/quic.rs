use crate::crypto;
use crate::crypto::NodeKeypair;
use crate::error::{Result, SyncError};
use crate::net::protocol::Message;
use quinn::rustls::client::danger::{
    HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier,
};
use quinn::rustls::{DigitallySignedStruct, RootCertStore, SignatureScheme};
use quinn::{ClientConfig, Endpoint, ServerConfig};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

pub const MAX_MESSAGE_SIZE: usize = 16 * 1024 * 1024; // 16 MB

const ALPN_PROTOCOL: &[u8] = b"p2rent/1";

/// pubkey (32) + timestamp (8) + ed25519 signature (64)
const HANDSHAKE_SIZE: usize = 32 + 8 + 64;
const MAX_HANDSHAKE_DRIFT_SECS: u64 = 60;

#[derive(Debug, Clone)]
pub struct Peer {
    pub id: crypto::NodeId,
    pub connection: quinn::Connection,
}

fn generate_self_signed_cert() -> Result<(CertificateDer<'static>, PrivateKeyDer<'static>)> {
    let certified_key = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])?;
    let cert_der = CertificateDer::from(certified_key.cert.der().to_vec());
    let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
        certified_key.signing_key.serialize_der(),
    ));
    Ok((cert_der, key_der))
}

fn current_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn build_handshake_bytes(keypair: &NodeKeypair) -> Result<Vec<u8>> {
    let node_id = crypto::node_id(keypair);
    let now = current_unix_secs();
    let payload = crypto::build_handshake_payload(&node_id, now);
    let signature = crypto::sign(keypair, &payload)?;

    let mut out = Vec::with_capacity(HANDSHAKE_SIZE);
    out.extend_from_slice(&keypair.verifying.to_bytes());
    out.extend_from_slice(&now.to_be_bytes());
    out.extend_from_slice(&signature);
    Ok(out)
}

fn verify_handshake(data: &[u8]) -> Result<crypto::NodeId> {
    if data.len() != HANDSHAKE_SIZE {
        return Err(SyncError::Other(format!(
            "invalid handshake: expected {} bytes, got {}",
            HANDSHAKE_SIZE,
            data.len()
        )));
    }

    let pubkey: [u8; 32] = data[0..32].try_into().unwrap();
    let timestamp = u64::from_be_bytes(data[32..40].try_into().unwrap());
    let sig: [u8; 64] = data[40..104].try_into().unwrap();

    let node_id = crypto::node_id_from_pubkey(&pubkey);
    let payload = crypto::build_handshake_payload(&node_id, timestamp);

    if !crypto::verify(&pubkey, &payload, &sig)? {
        return Err(SyncError::Other(
            "handshake signature verification failed".into(),
        ));
    }

    let now = current_unix_secs();
    let drift = now.abs_diff(timestamp);
    if drift > MAX_HANDSHAKE_DRIFT_SECS {
        return Err(SyncError::Other(format!(
            "handshake timestamp drift {drift}s exceeds {MAX_HANDSHAKE_DRIFT_SECS}s limit"
        )));
    }

    Ok(node_id)
}

pub struct QuicServer {
    endpoint: quinn::Endpoint,
    keypair: NodeKeypair,
}

impl QuicServer {
    pub async fn bind(addr: SocketAddr, keypair: NodeKeypair) -> Result<Self> {
        let (cert, key) = generate_self_signed_cert()?;
        let mut server_crypto = quinn::rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert], key)?;
        server_crypto.alpn_protocols = vec![ALPN_PROTOCOL.to_vec()];
        let quic_crypto = quinn::crypto::rustls::QuicServerConfig::try_from(server_crypto)
            .map_err(|e| SyncError::Other(format!("QUIC server crypto config: {e}")))?;
        let server_config = ServerConfig::with_crypto(Arc::new(quic_crypto));
        let endpoint = Endpoint::server(server_config, addr)?;
        Ok(Self { endpoint, keypair })
    }

    pub async fn accept_and_handshake(&self) -> Result<Peer> {
        let incoming = self
            .endpoint
            .accept()
            .await
            .ok_or_else(|| SyncError::Other("endpoint closed".into()))?;
        let conn = incoming.await?;
        let (mut send, mut recv) = conn.accept_bi().await?;

        let client_hello = receive_raw(&mut recv, HANDSHAKE_SIZE).await?;
        let client_id = verify_handshake(&client_hello)?;

        let server_hello = build_handshake_bytes(&self.keypair)?;
        send_raw(&mut send, &server_hello).await?;

        Ok(Peer {
            id: client_id,
            connection: conn,
        })
    }
}

pub struct QuicClient {
    endpoint: Endpoint,
}

impl QuicClient {
    pub async fn new() -> Result<Self> {
        #[derive(Debug)]
        struct SkipServerVerification;

        impl ServerCertVerifier for SkipServerVerification {
            fn verify_server_cert(
                &self,
                _end_entity: &CertificateDer<'_>,
                _intermediates: &[CertificateDer<'_>],
                _server_name: &rustls_pki_types::ServerName<'_>,
                _ocsp_response: &[u8],
                _now: rustls_pki_types::UnixTime,
            ) -> std::result::Result<ServerCertVerified, quinn::rustls::Error> {
                Ok(ServerCertVerified::assertion())
            }

            fn verify_tls12_signature(
                &self,
                _message: &[u8],
                _cert: &CertificateDer<'_>,
                _dss: &DigitallySignedStruct,
            ) -> std::result::Result<HandshakeSignatureValid, quinn::rustls::Error> {
                Ok(HandshakeSignatureValid::assertion())
            }

            fn verify_tls13_signature(
                &self,
                _message: &[u8],
                _cert: &CertificateDer<'_>,
                _dss: &DigitallySignedStruct,
            ) -> std::result::Result<HandshakeSignatureValid, quinn::rustls::Error> {
                Ok(HandshakeSignatureValid::assertion())
            }

            fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
                vec![
                    SignatureScheme::ECDSA_NISTP256_SHA256,
                    SignatureScheme::ED25519,
                    SignatureScheme::RSA_PSS_SHA256,
                    SignatureScheme::RSA_PKCS1_SHA256,
                ]
            }
        }

        let roots = RootCertStore::empty();
        let mut rustls_config = quinn::rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        rustls_config
            .dangerous()
            .set_certificate_verifier(Arc::new(SkipServerVerification));
        rustls_config.alpn_protocols = vec![ALPN_PROTOCOL.to_vec()];

        let crypto = quinn::crypto::rustls::QuicClientConfig::try_from(rustls_config)
            .map_err(|e| SyncError::Other(format!("QUIC client crypto config: {e}")))?;
        let client_config = ClientConfig::new(Arc::new(crypto));

        let mut endpoint = Endpoint::client("0.0.0.0:0".parse().unwrap())?;
        endpoint.set_default_client_config(client_config);

        Ok(Self { endpoint })
    }

    pub async fn connect_and_handshake(
        &self,
        addr: SocketAddr,
        keypair: &NodeKeypair,
    ) -> Result<Peer> {
        let conn = self.endpoint.connect(addr, "localhost")?.await?;
        let (mut send, mut recv) = conn.open_bi().await?;

        let client_hello = build_handshake_bytes(keypair)?;
        send_raw(&mut send, &client_hello).await?;

        let server_hello = receive_raw(&mut recv, HANDSHAKE_SIZE).await?;
        let server_id = verify_handshake(&server_hello)?;

        Ok(Peer {
            id: server_id,
            connection: conn,
        })
    }
}

async fn send_raw(stream: &mut quinn::SendStream, data: &[u8]) -> Result<()> {
    stream.write_all(data).await?;
    stream.finish()?;
    Ok(())
}

async fn receive_raw(stream: &mut quinn::RecvStream, max_size: usize) -> Result<Vec<u8>> {
    let data = stream.read_to_end(max_size).await?;
    Ok(data)
}

pub async fn send_message(stream: &mut quinn::SendStream, msg: &Message) -> Result<()> {
    let data =
        bincode::serialize(msg).map_err(|e| SyncError::Other(format!("bincode encode: {e}")))?;
    send_raw(stream, &data).await
}

pub async fn receive_message(stream: &mut quinn::RecvStream) -> Result<Message> {
    let data = receive_raw(stream, MAX_MESSAGE_SIZE).await?;
    let msg: Message = bincode::deserialize(&data)
        .map_err(|e| SyncError::Other(format!("bincode decode: {e}")))?;
    Ok(msg)
}
