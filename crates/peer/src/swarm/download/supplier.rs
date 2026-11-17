//! Another machine, as a supplier of chunks.
//!
//! This is all a download knows about the wire protocol: how to reach a
//! peer, ask what it holds, and ask for a chunk. Choosing chunks, checking
//! them and dropping a peer that lies are the engine's work.

use crate::error::Error;
use crate::net::{Connection, NodeId, PeerInfo};
use crate::swarm::Node;
use engine::sync;
use model::artifact::Artifact;
use model::place::{Capabilities, Failure, Pending, Status, Supplier, SupplierId, Supply};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tracing::debug;

/// Most peers a download will keep the address of.
const MAX_KNOWN_PEERS: usize = 512;

/// Every peer a download has come to know, with what it last heard of
/// where the peer is.
pub(super) type Known = Arc<Mutex<HashMap<NodeId, PeerInfo>>>;

/// Most addresses a download keeps of one peer that another peer told of.
const MAX_ADDRS_PER_PEER: usize = 8;

/// What every peer of one download is reached with.
#[derive(Debug, Clone)]
pub(super) struct Swarm {
    pub node: Arc<Node>,
    pub artifact: Arc<Artifact>,
    pub known: Known,
    /// The relays this download may reach a peer through: those the ticket
    /// names, and this node's own.
    pub relays: Arc<[String]>,
}

impl Swarm {
    /// A supplier for a peer that another peer told of.
    ///
    /// What a peer says of another is not to be trusted with where this
    /// node sends requests. A relay is a web address, and a peer that
    /// could name any would have every downloader call on a server of its
    /// choosing. So a relay is kept only if it is one this download uses
    /// already, and the addresses are kept to a few.
    fn introduced(&self, peer: PeerInfo) -> Arc<dyn Supplier> {
        self.supplier(as_far_as_trusted(peer, &self.relays))
    }

    /// A supplier for `peer`, which is remembered.
    pub(super) fn supplier(&self, peer: PeerInfo) -> Arc<dyn Supplier> {
        let mut known = sync::lock(&self.known);
        if known.len() < MAX_KNOWN_PEERS || known.contains_key(&peer.node) {
            known.insert(peer.node, peer.clone());
        }
        Arc::new(Peer {
            swarm: self.clone(),
            peer,
        })
    }
}

/// What another peer said of `peer`, as far as it is acted on: a relay only
/// if it is one of `relays`, and no more than a few addresses.
fn as_far_as_trusted(mut peer: PeerInfo, relays: &[String]) -> PeerInfo {
    peer.addrs.truncate(MAX_ADDRS_PER_PEER);
    peer.relay = peer.relay.filter(|relay| relays.contains(relay));
    peer
}

#[derive(Debug)]
struct Peer {
    swarm: Swarm,
    peer: PeerInfo,
}

impl Supplier for Peer {
    fn id(&self) -> SupplierId {
        *self.peer.node.as_bytes()
    }

    fn open(&self) -> Pending<'_, Arc<dyn Supply>> {
        Box::pin(async move {
            let endpoint = self.swarm.node.endpoint();
            let connection = endpoint
                .connect(&self.peer)
                .await
                .map_err(|e| unreachable(&e))?;
            debug!(peer = %connection.peer().short(), at = ?connection.peer_info(), "connected to peer");
            let link: Arc<dyn Supply> = Arc::new(Link {
                swarm: self.swarm.clone(),
                connection,
            });
            Ok(link)
        })
    }
}

/// A connection to a peer, closed when the download lets go of it.
#[derive(Debug)]
struct Link {
    swarm: Swarm,
    connection: Connection,
}

impl Supply for Link {
    /// Asking also keeps this node listed as a participant on the peer's
    /// side.
    fn status(&self) -> Pending<'_, Status> {
        Box::pin(async move {
            let artifact = &self.swarm.artifact;
            let status = self
                .connection
                .status(artifact.content_id())
                .await
                .map_err(|e| unreachable(&e))?
                .ok_or_else(|| Failure::Absent("peer does not serve this artifact".into()))?;
            let held = status
                .have
                .into_bitfield(artifact.chunk_count())
                .map_err(|e| Failure::Corrupt(e.to_string()))?;
            let others = status
                .peers
                .into_iter()
                .map(|peer| self.swarm.introduced(peer))
                .collect();
            Ok(Status { held, others })
        })
    }

    fn chunk(&self, index: u64) -> Pending<'_, Vec<u8>> {
        Box::pin(async move {
            let artifact = &self.swarm.artifact;
            let location = artifact
                .locate(index)
                .ok_or_else(|| Failure::Fatal("chunk index out of range".into()))?;
            self.connection
                .chunk(artifact.content_id(), index, location.len)
                .await
                .map_err(|e| unreachable(&e))?
                .ok_or_else(|| Failure::Absent(format!("peer does not hold chunk {index}")))
        })
    }

    /// A peer is asked for any chunk, and for several at once.
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            random_access: true,
        }
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        debug!(
            peer = %self.connection.peer().short(),
            path_mtu = ?self.connection.path_mtu(),
            "disconnected from peer"
        );
        self.connection.close();
    }
}

/// A peer that cannot be reached, or whose connection broke, is worth
/// trying again.
fn unreachable(error: &Error) -> Failure {
    Failure::Unreachable(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;

    #[test]
    fn a_peer_cannot_send_this_node_to_a_relay_of_its_choosing() {
        let ours = ["https://relay.example/".to_owned()];
        let addrs: Vec<SocketAddr> = (0..20u16)
            .map(|port| SocketAddr::from(([10, 0, 0, 1], port)))
            .collect();
        let told = |relay: Option<&str>| PeerInfo {
            node: NodeId::from_bytes([7; 32]),
            addrs: addrs.clone(),
            relay: relay.map(str::to_owned),
        };

        // A relay this download uses already is kept.
        let kept = as_far_as_trusted(told(Some("https://relay.example/")), &ours);
        assert_eq!(kept.relay.as_deref(), Some("https://relay.example/"));
        // Any other is dropped, and the node is still known by its id.
        let dropped = as_far_as_trusted(told(Some("http://192.168.1.1/admin")), &ours);
        assert_eq!(dropped.relay, None);
        assert_eq!(dropped.node, NodeId::from_bytes([7; 32]));
        assert_eq!(as_far_as_trusted(told(None), &ours).relay, None);
        assert_eq!(
            as_far_as_trusted(told(Some("https://relay.example/")), &[]).relay,
            None
        );

        // And only the first few addresses are.
        assert_eq!(kept.addrs, addrs[..MAX_ADDRS_PER_PEER]);
    }
}
