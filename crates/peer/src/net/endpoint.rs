use super::identity::Identity;
use super::proto::{self, ALPN, Request, Response, Status};
use crate::error::{Error, Result};
use iroh::endpoint::{
    MtuDiscoveryConfig, Path, QuicTransportConfig, RecvStream, SendStream, VarInt, presets,
};
use iroh::{EndpointAddr, EndpointId, TransportAddr};
use model::artifact::{Hash, MAX_MANIFEST_BYTES};
use model::ticket::NodeId;
use std::net::SocketAddr;
use std::time::Duration;

const KEEP_ALIVE: Duration = Duration::from_secs(5);
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_STREAMS_PER_CONNECTION: u32 = 16;
const STREAM_WINDOW: u32 = 8 * 1024 * 1024;
const CONNECTION_WINDOW: u32 = 64 * 1024 * 1024;

/// Largest datagram to try to send: what fits in a 9000-byte jumbo frame
/// after the IPv6 and UDP headers.
///
/// Most of the CPU cost of a transfer is paid per packet, so bigger packets
/// are the cheapest speed-up there is. Connections start at QUIC's safe
/// minimum and only grow as far as path MTU discovery proves the route and
/// the other end can carry. An iroh endpoint takes datagrams of up to 1472
/// bytes, and offers no way to raise that, so that is as far as they grow
/// today.
const MAX_DATAGRAM: u16 = 9000 - 40 - 8;

/// A socket that both dials and accepts, authenticated as one node.
///
/// A node is dialled by its id: the connection is made only with whoever
/// holds the key that the id is the public half of.
#[derive(Debug, Clone)]
pub struct Endpoint {
    inner: iroh::Endpoint,
    node_id: NodeId,
    bound: SocketAddr,
}

impl Endpoint {
    /// Binds `addr`, and only it.
    ///
    /// # Errors
    ///
    /// Returns an error if `addr` cannot be bound.
    pub async fn bind(identity: &Identity, addr: SocketAddr) -> Result<Self> {
        let mut mtu_discovery = MtuDiscoveryConfig::default();
        mtu_discovery.upper_bound(MAX_DATAGRAM);
        let idle_timeout = IDLE_TIMEOUT
            .try_into()
            .map_err(|_| Error::Transport("idle timeout out of range".into()))?;
        let transport = QuicTransportConfig::builder()
            .max_concurrent_bidi_streams(VarInt::from_u32(MAX_STREAMS_PER_CONNECTION))
            .max_concurrent_uni_streams(VarInt::from_u32(0))
            .keep_alive_interval(KEEP_ALIVE)
            .max_idle_timeout(Some(idle_timeout))
            .stream_receive_window(VarInt::from_u32(STREAM_WINDOW))
            .receive_window(VarInt::from_u32(CONNECTION_WINDOW))
            .send_window(u64::from(CONNECTION_WINDOW))
            .mtu_discovery_config(Some(mtu_discovery))
            .build();

        let inner = iroh::Endpoint::builder(presets::Minimal)
            .secret_key(identity.secret().clone())
            .alpns(vec![ALPN.to_vec()])
            .transport_config(transport)
            .clear_ip_transports()
            .bind_addr(addr)
            .map_err(|e| Error::Transport(e.to_string()))?
            .bind()
            .await
            .map_err(|e| Error::Transport(e.to_string()))?;
        let bound = inner.bound_sockets().into_iter().next().unwrap_or(addr);
        Ok(Self {
            inner,
            node_id: identity.node_id(),
            bound,
        })
    }

    /// The node id this endpoint authenticates as.
    pub fn node_id(&self) -> NodeId {
        self.node_id
    }

    /// The address the socket is bound to.
    pub fn local_addr(&self) -> SocketAddr {
        self.bound
    }

    /// Dials `expected` at `addr`. The connection is made only if whoever
    /// answers there proves to be that node.
    ///
    /// # Errors
    ///
    /// Returns a transport error if nobody answers at `addr`, or if whoever
    /// does is another node.
    pub async fn connect(&self, addr: SocketAddr, expected: NodeId) -> Result<Connection> {
        let id = EndpointId::from_bytes(expected.as_bytes())
            .map_err(|e| Error::Identity(format!("{expected} is not the id of a node: {e}")))?;
        let remote = EndpointAddr::new(id).with_ip_addr(addr);
        let inner = self.inner.connect(remote, ALPN).await.map_err(|e| {
            let node = expected.short();
            Error::Transport(format!("cannot connect to node {node} at {addr}: {e}"))
        })?;
        Ok(Connection {
            inner,
            peer: expected,
        })
    }

    /// Waits for the next inbound connection attempt. Returns `None` once the
    /// endpoint is closed.
    pub async fn accept(&self) -> Option<Incoming> {
        self.inner.accept().await.map(Incoming)
    }

    /// Closes every connection and stops accepting new ones. Returns once
    /// the other ends have been told, or have not answered for a while.
    pub async fn close(&self) {
        self.inner.close().await;
    }
}

/// An inbound connection whose handshake has not completed yet.
#[derive(Debug)]
pub struct Incoming(iroh::endpoint::Incoming);

impl Incoming {
    /// Completes the handshake. Run this off the accept loop so a slow or
    /// hostile client cannot hold up other connections.
    ///
    /// # Errors
    ///
    /// Returns an error if the handshake fails.
    pub async fn establish(self) -> Result<Connection> {
        let inner = self.0.await.map_err(|e| Error::Transport(e.to_string()))?;
        let peer = NodeId::from_bytes(*inner.remote_id().as_bytes());
        Ok(Connection { inner, peer })
    }
}

/// An established connection to a peer whose node id is known.
#[derive(Debug, Clone)]
pub struct Connection {
    inner: iroh::endpoint::Connection,
    peer: NodeId,
}

impl Connection {
    /// The node id the peer proved during the handshake.
    pub fn peer(&self) -> NodeId {
        self.peer
    }

    /// The peer's address as seen from this side, if the connection goes
    /// straight to it. Peers dial and listen on the same socket, so this is
    /// also where others can reach it.
    pub fn remote_addr(&self) -> Option<SocketAddr> {
        let paths = self.inner.paths();
        let selected = paths.iter().find(Path::is_selected)?;
        match selected.remote_addr() {
            // A dual-stack socket reports IPv4 peers as IPv4-mapped IPv6.
            TransportAddr::Ip(addr) => Some(SocketAddr::new(addr.ip().to_canonical(), addr.port())),
            _ => None,
        }
    }

    /// The largest datagram the route to the peer is currently known to
    /// carry. Starts at QUIC's minimum and grows as discovery probes succeed.
    pub fn path_mtu(&self) -> Option<u16> {
        let paths = self.inner.paths();
        let selected = paths.iter().find(Path::is_selected)?;
        Some(selected.stats().current_mtu)
    }

    /// Closes the connection.
    pub fn close(&self) {
        self.inner.close(VarInt::from_u32(0), b"done");
    }

    /// Waits for the peer to open a request stream.
    ///
    /// # Errors
    ///
    /// Returns a transport error once the connection is closed.
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
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or the peer answers with something other than a
    /// manifest.
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

    /// Asks which chunks of `content` the peer holds and which other peers it knows. `None`
    /// means the peer does not hold this content.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or the answer is not a status.
    pub async fn status(&self, content: Hash) -> Result<Option<Status>> {
        match self.request(&Request::Status { content }).await? {
            (Response::Status(status), _) => Ok(Some(status)),
            (Response::NotFound, _) => Ok(None),
            (other, _) => Err(unexpected(&other)),
        }
    }

    /// Fetches one chunk, which must be exactly `expected_len` bytes. `None`
    /// means the peer does not hold it. The bytes are unverified.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Protocol`] if the peer announces a different length, and a transport error
    /// if the request fails.
    pub async fn chunk(
        &self,
        content: Hash,
        index: u64,
        expected_len: u32,
    ) -> Result<Option<Vec<u8>>> {
        match self.request(&Request::Chunk { content, index }).await? {
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
