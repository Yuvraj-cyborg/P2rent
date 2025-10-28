use crate::crypto;
use crate::crypto::NodeKeypair;
use crate::error::Result;
use crate::protocol::Message;
use quinn::{ClientConfig, Endpoint, ServerConfig};
use quinn::rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use quinn::rustls::{DigitallySignedStruct, RootCertStore, SignatureScheme};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub struct Peer {
    pub id: crypto::NodeId,
    pub connection: quinn::Connection,
}

// Need to generate a TLS certificate for the server using Pkcs8, faced issue in enum type of Pkcs8 but now its fixed.
// Self note : PrivateKeyDer::Pkcs8() expects enum of PrivatePkcs8KeyDer.
fn generate_self_signed_cert() -> Result<(CertificateDer<'static>, PrivateKeyDer<'static>)> {
    let certified_key = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])?;
    let cert_der =  CertificateDer::from(certified_key.cert.der().to_vec());

    let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(certified_key.signing_key.serialize_der()));

    Ok((cert_der, key_der))
}

pub struct QuicServer {
    endpoint: quinn::Endpoint,
    keypair: NodeKeypair,
}

impl QuicServer {
    pub async fn bind(addr: SocketAddr, keypair: NodeKeypair) -> Result<Self> {
        let (cert, key) = generate_self_signed_cert()?;
        let server_config = ServerConfig::with_single_cert(vec![cert], key)?;
        let endpoint = Endpoint::server(server_config, addr)?;

        Ok(Self { endpoint, keypair })
    }

    // The handshake logic for now, might change later
    pub async fn accept_and_handshake(&self) -> Result<Peer> {
        let incoming_conn = self.endpoint.accept().await.ok_or_else(|| crate::error::SyncError::Other("Endpoint closed".into()))?;
        let conn = incoming_conn.await?;
        let (mut send, mut recv) = conn.accept_bi().await?;
        let client_hello = receive_message(&mut recv).await?;
        if client_hello.len() < 64 + 8 + 64 { return Err(crate::error::SyncError::Other("Invalid handshake".into())); }
        let client_id = String::from_utf8(client_hello[0..64].to_vec())?;
        let our_id = crypto::node_id(&self.keypair);
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let payload = crypto::build_handshake_payload(&our_id, now);
        let signature = crypto::sign(&self.keypair, &payload)?;
        let mut server_hello = Vec::new();
        server_hello.extend_from_slice(our_id.as_bytes());
        server_hello.extend_from_slice(&now.to_be_bytes());
        server_hello.extend_from_slice(&signature);
        send_message(&mut send, &server_hello).await?;
        Ok(Peer { id: client_id, connection: conn })
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

        rustls_config.alpn_protocols = vec![b"hq-29".to_vec()];

        let crypto = quinn::crypto::rustls::QuicClientConfig::try_from(rustls_config).unwrap();

        let client_config = ClientConfig::new(Arc::new(crypto));

        let mut endpoint = Endpoint::client("0.0.0.0:0".parse().unwrap())?;
        endpoint.set_default_client_config(client_config);

        Ok(Self { endpoint })
    }

    // The handshake logic is self-contained and complete.
    pub async fn connect_and_handshake(&self, addr: SocketAddr, keypair: &NodeKeypair) -> Result<Peer> {
        let conn = self.endpoint.connect(addr, "localhost")?.await?;
        let (mut send, mut recv) = conn.open_bi().await?;
        let our_id = crypto::node_id(keypair);
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let payload = crypto::build_handshake_payload(&our_id, now);
        let signature = crypto::sign(keypair, &payload)?;
        let mut client_hello = Vec::new();
        client_hello.extend_from_slice(our_id.as_bytes());
        client_hello.extend_from_slice(&now.to_be_bytes());
        client_hello.extend_from_slice(&signature);
        send_message(&mut send, &client_hello).await?;
        let server_hello = receive_message(&mut recv).await?;
        if server_hello.len() < 64 + 8 + 64 { return Err(crate::error::SyncError::Other("Invalid handshake".into())); }
        let server_id = String::from_utf8(server_hello[0..64].to_vec())?;
        Ok(Peer { id: server_id, connection: conn })
    }
}

pub async fn send_message(stream: &mut quinn::SendStream, msg: &[u8]) -> Result<()> {
    stream.write_all(msg).await?;
    stream.finish()?;
    Ok(())
}

pub async fn receive_message(stream: &mut quinn::RecvStream) -> Result<Vec<u8>> {
    let data = stream.read_to_end(usize::MAX).await?;
    Ok(data)
}

pub async fn send_json_message(stream: &mut quinn::SendStream, msg: &Message) -> Result<()> {
    let data = serde_json::to_vec(msg)?;
    send_message(stream, &data).await
}

pub async fn receive_json_message(stream: &mut quinn::RecvStream) -> Result<Message> {
    let data = receive_message(stream).await?;
    let msg: Message = serde_json::from_slice(&data)?;
    Ok(msg)
}
