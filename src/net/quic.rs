use crate::crypto;
use crate::crypto::NodeKeypair;
use crate::error::Result;
use quinn::{Endpoint, ServerConfig};
use rustls::pki_types::{CertificateDer, PrivateKeyDer,PrivatePkcs8KeyDer};
use std::net::SocketAddr ;
use rustls::client::danger::{ServerCertVerified, ServerCertVerifier};
use std::sync::Arc;
use std::time;

#[derive(Debug, Clone)]
pub struct Peer {
    pub id: NodeKeypair,
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
        let our_id = /* Call your node_id function */;
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let payload = /* Call your build_handshake_payload function */;
        let signature = /* Call your sign function */;
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
                    end_entity: &CertificateDer<'_>,
                    intermediates: &[CertificateDer<'_>],
                    server_name: &rustls_pki_types::ServerName<'_>,
                    ocsp_response: &[u8],
                    now: rustls_pki_types::UnixTime,
                ) -> std::result::Result<ServerCertVerified, rustls::Error> {
            Ok(ServerCertVerified::assertion())
            }

        }

        // 6. Build the rustls client configuration.
        let mut rustls_config = /* Call rustls::ClientConfig::builder() */
            // 7. Chain the call to `.with_safe_defaults()`.
            // 8. Chain the call to `.with_custom_certificate_verifier(...)`, passing it your SkipServerVerification struct.
            // 9. Chain the final call to `.with_no_client_auth()`.

        rustls_config.alpn_protocols = vec![b"hq-29".to_vec()];

        // 10. Wrap the rustls config for Quinn.
        let crypto = /* Call quinn::crypto::rustls::QuicClientConfig::try_from with the rustls_config */;

        // 11. Create the final Quinn config.
        let client_config = /* Call ClientConfig::new with the wrapped crypto config */;

        let mut endpoint = /* Call Endpoint::client with a bind address like "0.0.0.0:0" */;
        endpoint.set_default_client_config(client_config);

        Ok(Self { endpoint })
    }

    // The handshake logic is self-contained and complete.
    pub async fn connect_and_handshake(&self, addr: /* SocketAddr type */, keypair: & /* NodeKeypair type */) -> Result<Peer> {
        let conn = self.endpoint.connect(addr, "localhost")?.await?;
        let (mut send, mut recv) = conn.open_bi().await?;
        let our_id = /* Call your node_id function */;
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let payload = /* Call your build_handshake_payload function */;
        let signature = /* Call your sign function */;
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

pub async fn send_message(stream: &mut /* SendStream type */, msg: &[u8]) -> Result<()> {
    stream.write_all(msg).await?;
    stream.finish()?;
    Ok(())
}

pub async fn receive_message(stream: &mut /* RecvStream type */) -> Result<Vec<u8>> {
    let data = stream.read_to_end(usize::MAX).await?;
    Ok(data)
}
