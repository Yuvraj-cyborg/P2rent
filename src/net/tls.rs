//! TLS configuration in which a peer's certificate is nothing more than a
//! carrier for its Ed25519 public key.
//!
//! There is no certificate authority. Both sides present a self-signed
//! certificate, rustls checks that the handshake was signed by the key inside
//! it, and the caller then compares that key with the node id it expected.

use super::identity::{Identity, NodeId};
use super::proto::ALPN;
use crate::error::{Error, Result};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{DigitallySignedStruct, DistinguishedName, SignatureScheme};
use std::sync::Arc;

#[derive(Debug)]
struct NodeVerifier {
    algorithms: WebPkiSupportedAlgorithms,
}

impl NodeVerifier {
    fn require_node_certificate(certificate: &CertificateDer<'_>) -> Result<(), rustls::Error> {
        NodeId::from_certificate(certificate)
            .map(drop)
            .map_err(|e| rustls::Error::General(e.to_string()))
    }

    fn verify_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, certificate, dss, &self.algorithms)
    }
}

fn tls12_unsupported() -> rustls::Error {
    rustls::Error::General("TLS 1.2 is not supported".into())
}

impl ServerCertVerifier for NodeVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Self::require_node_certificate(end_entity)?;
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(tls12_unsupported())
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.verify_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ED25519]
    }
}

impl ClientCertVerifier for NodeVerifier {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        Self::require_node_certificate(end_entity)?;
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(tls12_unsupported())
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.verify_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ED25519]
    }
}

/// Server and client configurations that both authenticate as `identity`.
pub(crate) fn configs(identity: &Identity) -> Result<(quinn::ServerConfig, quinn::ClientConfig)> {
    let provider: Arc<CryptoProvider> = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = Arc::new(NodeVerifier {
        algorithms: provider.signature_verification_algorithms,
    });
    let (certificate, key) = identity.tls_material()?;

    let mut server = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_client_cert_verifier(verifier.clone())
        .with_single_cert(vec![certificate.clone()], key.clone_key())?;
    server.alpn_protocols = vec![ALPN.to_vec()];

    let mut client = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(vec![certificate], key)?;
    client.alpn_protocols = vec![ALPN.to_vec()];

    let server = quinn::crypto::rustls::QuicServerConfig::try_from(server)
        .map_err(|e| Error::Transport(e.to_string()))?;
    let client = quinn::crypto::rustls::QuicClientConfig::try_from(client)
        .map_err(|e| Error::Transport(e.to_string()))?;
    Ok((
        quinn::ServerConfig::with_crypto(Arc::new(server)),
        quinn::ClientConfig::new(Arc::new(client)),
    ))
}
