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
use model::place::{Arrived, Capabilities, Failure, Pending, Status, Supplier, SupplierId, Supply};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tracing::debug;

/// Most peers a download will keep the address of.
const MAX_KNOWN_PEERS: usize = 512;

/// Every peer a download has come to know, with where it was last seen.
pub(super) type Known = Arc<Mutex<HashMap<NodeId, SocketAddr>>>;

/// What every peer of one download is reached with.
#[derive(Debug, Clone)]
pub(super) struct Swarm {
    pub node: Arc<Node>,
    pub artifact: Arc<Artifact>,
    pub known: Known,
}

impl Swarm {
    /// A supplier for `peer`, which is remembered.
    pub(super) fn supplier(&self, peer: PeerInfo) -> Arc<dyn Supplier> {
        let mut known = sync::lock(&self.known);
        if known.len() < MAX_KNOWN_PEERS || known.contains_key(&peer.node) {
            known.insert(peer.node, peer.addr);
        }
        Arc::new(Peer {
            swarm: self.clone(),
            peer,
        })
    }
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
                .connect(self.peer.addr, self.peer.node)
                .await
                .map_err(|e| unreachable(&e))?;
            debug!(peer = %connection.peer().short(), addr = %connection.remote_addr(), "connected to peer");
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
                .map(|peer| self.swarm.supplier(peer))
                .collect();
            Ok(Status { held, others })
        })
    }

    fn chunk<'a>(&'a self, index: u64, arrived: &'a Arrived) -> Pending<'a, Vec<u8>> {
        Box::pin(async move {
            let artifact = &self.swarm.artifact;
            let location = artifact
                .locate(index)
                .ok_or_else(|| Failure::Fatal("chunk index out of range".into()))?;
            self.connection
                .chunk(artifact.content_id(), index, location.len, arrived)
                .await
                .map_err(|e| unreachable(&e))?
                .ok_or_else(|| Failure::Absent(format!("peer does not hold chunk {index}")))
        })
    }

    /// Each chunk is a request of its own, so nothing is left open.
    fn leave(&self, _index: u64) {}

    /// A peer is asked for any chunk, and for several at once.
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            random_access: true,
            // Each request is a stream of the one connection to the peer.
            multiplexed: true,
        }
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        debug!(
            peer = %self.connection.peer().short(),
            path_mtu = self.connection.path_mtu(),
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
