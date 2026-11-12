use super::identity::{Identity, NodeId};
use super::proto::{self, Request, Response, Status};
use super::tls;
use crate::artifact::{Hash, MAX_MANIFEST_BYTES};
use crate::error::{Error, Result};
use quinn::{RecvStream, SendStream, VarInt};
use rustls::pki_types::CertificateDer;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

const KEEP_ALIVE: Duration = Duration::from_secs(5);
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_STREAMS_PER_CONNECTION: u32 = 16;
const STREAM_WINDOW: u32 = 8 * 1024 * 1024;
const CONNECTION_WINDOW: u32 = 64 * 1024 * 1024;

/// A QUIC socket that both dials and accepts, authenticated as one node.
#[derive(Debug, Clone)]
pub struct Endpoint {
    inner: quinn::Endpoint,
    node_id: NodeId,
}

impl Endpoint {
    /// Must be called from within a Tokio runtime.
    pub fn bind(identity: &Identity, addr: SocketAddr) -> Result<Self> {
        let (mut server, mut client) = tls::configs(identity)?;

        let mut transport = quinn::TransportConfig::default();
        transport
            .max_concurrent_bidi_streams(VarInt::from_u32(MAX_STREAMS_PER_CONNECTION))
            .max_concurrent_uni_streams(VarInt::from_u32(0))
            .keep_alive_interval(Some(KEEP_ALIVE))
            .max_idle_timeout(Some(
                VarInt::from_u32(IDLE_TIMEOUT.as_millis() as u32).into(),
            ))
            .stream_receive_window(VarInt::from_u32(STREAM_WINDOW))
            .receive_window(VarInt::from_u32(CONNECTION_WINDOW))
            .send_window(u64::from(CONNECTION_WINDOW));
        let transport = Arc::new(transport);
        server.transport_config(transport.clone());
        client.transport_config(transport);

        let mut inner = quinn::Endpoint::server(server, addr)?;
        inner.set_default_client_config(client);
        Ok(Self {
            inner,
            node_id: identity.node_id(),
        })
    }

    pub fn node_id(&self) -> NodeId {
        self.node_id
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.inner.local_addr()?)
    }

    /// Dials `addr` and succeeds only if the peer proves it is `expected`.
    pub async fn connect(&self, addr: SocketAddr, expected: NodeId) -> Result<Connection> {
        let inner = self.inner.connect(addr, "p2rent")?.await?;
        let connection = Connection::authenticated(inner)?;
        if connection.peer != expected {
            connection
                .inner
                .close(VarInt::from_u32(1), b"unexpected node id");
            return Err(Error::IdentityMismatch {
                expected: expected.to_string(),
                actual: connection.peer.to_string(),
            });
        }
        Ok(connection)
    }

    /// Waits for the next inbound connection attempt. Returns `None` once the
    /// endpoint is closed.
    pub async fn accept(&self) -> Option<Incoming> {
        self.inner.accept().await.map(Incoming)
    }

    /// Closes every connection and stops accepting new ones.
    pub fn close(&self) {
        self.inner.close(VarInt::from_u32(0), b"shutting down");
    }
}

/// An inbound connection whose handshake has not completed yet.
pub struct Incoming(quinn::Incoming);

impl Incoming {
    /// Completes the handshake. Run this off the accept loop so a slow or
    /// hostile client cannot hold up other connections.
    pub async fn establish(self) -> Result<Connection> {
        Connection::authenticated(self.0.await?)
    }
}

/// An established connection to a peer whose node id is known.
#[derive(Debug, Clone)]
pub struct Connection {
    inner: quinn::Connection,
    peer: NodeId,
}

impl Connection {
    fn authenticated(inner: quinn::Connection) -> Result<Self> {
        let certificates = inner
            .peer_identity()
            .and_then(|identity| identity.downcast::<Vec<CertificateDer<'static>>>().ok())
            .ok_or_else(|| Error::Identity("peer presented no certificate".into()))?;
        let leaf = certificates
            .first()
            .ok_or_else(|| Error::Identity("peer presented no certificate".into()))?;
        let peer = NodeId::from_certificate(leaf)?;
        Ok(Self { inner, peer })
    }

    pub fn peer(&self) -> NodeId {
        self.peer
    }

    /// The peer's address as seen from this side. Peers dial and listen on
    /// the same socket, so this is also where others can reach it.
    pub fn remote_addr(&self) -> SocketAddr {
        let addr = self.inner.remote_address();
        // A dual-stack socket reports IPv4 peers as IPv4-mapped IPv6.
        SocketAddr::new(addr.ip().to_canonical(), addr.port())
    }

    pub fn close(&self) {
        self.inner.close(VarInt::from_u32(0), b"done");
    }

    /// Waits for the peer to open a request stream.
    pub async fn accept_request(&self) -> Result<(SendStream, RecvStream)> {
        Ok(self.inner.accept_bi().await?)
    }

    async fn request(&self, request: &Request) -> Result<(Response, RecvStream)> {
        let (mut send, mut recv) = self.inner.open_bi().await?;
        proto::write_frame(&mut send, request).await?;
        send.finish()?;
        let response = proto::read_frame(&mut recv, proto::MAX_RESPONSE_BYTES).await?;
        Ok((response, recv))
    }

    /// Fetches the encoded manifest. `None` means the peer does not serve
    /// this artifact. The bytes are unverified.
    pub async fn manifest(&self, artifact: Hash) -> Result<Option<Vec<u8>>> {
        match self.request(&Request::Manifest { artifact }).await? {
            (Response::Manifest { len }, mut recv) => {
                let len = usize::try_from(len)
                    .ok()
                    .filter(|&len| len <= MAX_MANIFEST_BYTES)
                    .ok_or_else(|| Error::Protocol("manifest exceeds the size limit".into()))?;
                Ok(Some(proto::read_payload(&mut recv, len).await?))
            }
            (Response::NotFound, _) => Ok(None),
            (other, _) => Err(unexpected(&other)),
        }
    }

    pub async fn status(&self, artifact: Hash) -> Result<Option<Status>> {
        match self.request(&Request::Status { artifact }).await? {
            (Response::Status(status), _) => Ok(Some(status)),
            (Response::NotFound, _) => Ok(None),
            (other, _) => Err(unexpected(&other)),
        }
    }

    /// Fetches one chunk, which must be exactly `expected_len` bytes. `None`
    /// means the peer does not hold it. The bytes are unverified.
    pub async fn chunk(
        &self,
        artifact: Hash,
        index: u64,
        expected_len: u32,
    ) -> Result<Option<Vec<u8>>> {
        match self.request(&Request::Chunk { artifact, index }).await? {
            (Response::Chunk { len }, mut recv) if len == expected_len => {
                Ok(Some(proto::read_payload(&mut recv, len as usize).await?))
            }
            (Response::Chunk { len }, _) => Err(Error::Protocol(format!(
                "chunk {index} has length {len}, expected {expected_len}"
            ))),
            (Response::NotFound, _) => Ok(None),
            (other, _) => Err(unexpected(&other)),
        }
    }
}

fn unexpected(response: &Response) -> Error {
    let kind = match response {
        Response::Manifest { .. } => "manifest",
        Response::Status(_) => "status",
        Response::Chunk { .. } => "chunk",
        Response::NotFound => "not-found",
    };
    Error::Protocol(format!("unexpected {kind} response"))
}
