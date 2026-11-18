use super::identity::Identity;
use super::proto::{self, ALPN, PeerInfo, Request, Response, Status};
use crate::error::{Error, Result};
use iroh::endpoint::{
    MtuDiscoveryConfig, Path, QuicTransportConfig, RecvStream, SendStream, VarInt, presets,
};
use iroh::{EndpointAddr, EndpointId, RelayMode, RelayUrl, TransportAddr};
use model::artifact::{Hash, MAX_MANIFEST_BYTES};
use model::pace;
use model::place::Arrived;
use model::ticket::NodeId;
use noq_proto::congestion::Bbr3Config;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

const KEEP_ALIVE: Duration = Duration::from_secs(5);
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);
/// Streams a peer may have open at once: every request the engine keeps in
/// flight to one supplier, and a few for what else is asked meanwhile.
const MAX_STREAMS_PER_CONNECTION: u32 = pace::MOST_REQUESTS + 8;
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

/// How a node is reached when it cannot be dialled at an address: behind
/// a router, for the most part.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Relays {
    /// Through the public relays run by the makers of iroh. The node also
    /// lists itself in their directory, and looks others up there, so that
    /// a node id is enough to reach a node.
    #[default]
    Public,
    /// Through the relay at this web address, and no directory: a ticket
    /// has to say where a node is.
    At(String),
    /// Through no relay. A node is reached at an address or not at all.
    Off,
}

/// How a connection decides how fast it may send.
///
/// It is the choice of whoever sends: the machine that holds the data.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Congestion {
    /// Takes every lost packet for a sign of a full link and halves its
    /// rate. On a link that loses packets for other reasons, such as
    /// Wi-Fi or a long route, it stays far below what the link carries.
    #[default]
    Cubic,
    /// Measures what the link carries and holds that rate, whatever is
    /// lost: BBR, in its third version. Many times faster where packets
    /// are lost.
    Bbr,
}

/// How an endpoint is set up.
#[derive(Debug, Clone)]
pub struct Options {
    /// The address to listen on. Every address, at a port the system
    /// picks, if `None`.
    pub listen: Option<SocketAddr>,
    /// How the node is reached when it cannot be dialled.
    pub relays: Relays,
    /// Whether to open sockets at all. Without them, packets go only by
    /// way of a relay, and no other node learns this one's address.
    pub direct: bool,
    /// How the node's connections decide how fast they may send.
    pub congestion: Congestion,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            listen: None,
            relays: Relays::default(),
            direct: true,
            congestion: Congestion::default(),
        }
    }
}

/// A socket that both dials and accepts, authenticated as one node.
///
/// A node is dialled by its id: the connection is made only with whoever
/// holds the key that the id is the public half of.
#[derive(Debug, Clone)]
pub struct Endpoint {
    inner: iroh::Endpoint,
    node_id: NodeId,
    /// Whether the node is to be reached through a relay.
    relayed: bool,
}

impl Endpoint {
    /// Opens the sockets, and begins to reach for a relay if there is to
    /// be one.
    ///
    /// # Errors
    ///
    /// Returns an error if the address to listen on cannot be bound, or if
    /// a relay's address is not one.
    pub async fn bind(identity: &Identity, options: &Options) -> Result<Self> {
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
            .mtu_discovery_config(Some(mtu_discovery));
        let transport = match options.congestion {
            // What the library does unless told otherwise.
            Congestion::Cubic => transport,
            Congestion::Bbr => {
                transport.congestion_controller_factory(Arc::new(Bbr3Config::default()))
            }
        };
        let transport = transport.build();

        let mut builder = match &options.relays {
            Relays::Public => iroh::Endpoint::builder(presets::N0),
            Relays::At(url) => iroh::Endpoint::builder(presets::Minimal)
                .relay_mode(RelayMode::custom([relay_url(url)?])),
            Relays::Off => iroh::Endpoint::builder(presets::Minimal),
        };
        builder = builder
            .secret_key(identity.secret().clone())
            .alpns(vec![ALPN.to_vec()])
            .transport_config(transport);
        if !options.direct {
            builder = builder.clear_ip_transports();
        } else if let Some(addr) = options.listen {
            let unusable = |e: &dyn std::fmt::Display| Error::Transport(e.to_string());
            builder = builder
                .clear_ip_transports()
                .bind_addr(addr)
                .map_err(|e| unusable(&e))?;
        }
        let inner = builder
            .bind()
            .await
            .map_err(|e| Error::Transport(e.to_string()))?;
        Ok(Self {
            inner,
            node_id: identity.node_id(),
            relayed: options.relays != Relays::Off,
        })
    }

    /// The node id this endpoint authenticates as.
    pub fn node_id(&self) -> NodeId {
        self.node_id
    }

    /// The addresses the sockets are bound to: one for IPv4 and one for
    /// IPv6, unless one address was asked for. None, if the endpoint has no
    /// sockets.
    pub fn local_addrs(&self) -> Vec<SocketAddr> {
        self.inner.bound_sockets()
    }

    /// Waits until a relay has been reached. Returns at once if there is to
    /// be none.
    pub async fn online(&self) {
        if self.relayed {
            self.inner.online().await;
        }
    }

    /// The relay this node is reached through, once one has been reached.
    pub fn relay(&self) -> Option<String> {
        let addr = self.inner.addr();
        addr.relay_urls().next().map(ToString::to_string)
    }

    /// Dials `peer`. The connection is made only if whoever answers proves
    /// to be that node.
    ///
    /// What is known of where the peer is, is tried first. With nothing
    /// known, or nobody there, the node is looked up by its id, if this
    /// endpoint looks nodes up.
    ///
    /// # Errors
    ///
    /// Returns a transport error if the node cannot be reached, or if
    /// whoever answers is another node.
    pub async fn connect(&self, peer: &PeerInfo) -> Result<Connection> {
        let expected = peer.node;
        let id = EndpointId::from_bytes(expected.as_bytes())
            .map_err(|e| Error::Identity(format!("{expected} is not the id of a node: {e}")))?;
        let mut remote = EndpointAddr::new(id);
        for &addr in &peer.addrs {
            remote = remote.with_ip_addr(addr);
        }
        if let Some(relay) = &peer.relay {
            remote = remote.with_relay_url(relay_url(relay)?);
        }
        let inner = self.inner.connect(remote, ALPN).await.map_err(|e| {
            Error::Transport(format!("cannot connect to node {}: {e}", expected.short()))
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

    /// The peer, and where it is as seen from this side: the addresses the
    /// connection goes straight to, and the relay it goes by way of. Peers
    /// dial and listen on the same socket, so this is also where others can
    /// reach it.
    pub fn peer_info(&self) -> PeerInfo {
        let mut info = PeerInfo::of(self.peer);
        for path in self.inner.paths().iter() {
            match path.remote_addr() {
                // A dual-stack socket reports IPv4 peers as IPv4-mapped
                // IPv6.
                TransportAddr::Ip(addr) => {
                    let addr = SocketAddr::new(addr.ip().to_canonical(), addr.port());
                    info.addrs.push(addr);
                }
                TransportAddr::Relay(url) => info.relay = Some(url.to_string()),
                _ => {}
            }
        }
        info
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
                let arrived = Arrived::default();
                Ok(Some(proto::read_payload(&mut recv, len, &arrived).await?))
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
    /// means the peer does not hold it. The bytes are unverified, and are
    /// added to `arrived` as they come.
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
        arrived: &Arrived,
    ) -> Result<Option<Vec<u8>>> {
        match self.request(&Request::Chunk { content, index }).await? {
            (Response::Chunk { len }, mut recv) if len == expected_len => {
                let whole = proto::read_payload(&mut recv, len as usize, arrived);
                Ok(Some(whole.await?))
            }
            (Response::Chunk { len }, _) => Err(Error::Protocol(format!(
                "chunk {index} has length {len}, expected {expected_len}"
            ))),
            (Response::NotFound, _) => Ok(None),
            (other, _) => Err(unexpected(&other)),
        }
    }
}

/// Reads the web address of a relay.
fn relay_url(text: &str) -> Result<RelayUrl> {
    text.parse()
        .map_err(|e| Error::Transport(format!("{text:?} is not the address of a relay: {e}")))
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
