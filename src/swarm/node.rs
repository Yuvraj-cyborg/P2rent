use super::server;
use super::share::Share;
use super::ticket::{Ticket, TicketPeer};
use crate::artifact::{Artifact, Bitfield, Hash};
use crate::error::Result;
use crate::net::{Endpoint, Identity, NodeId};
use crate::store::DiskStore;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

/// A participant in one or more swarms: one socket, one identity, and the
/// set of artifacts it currently offers.
#[derive(Debug)]
pub struct Node {
    endpoint: Endpoint,
    shares: RwLock<HashMap<Hash, Arc<Share>>>,
}

impl Node {
    /// Binds `addr` and starts answering peers. Must be called from within a
    /// Tokio runtime.
    pub fn bind(identity: &Identity, addr: SocketAddr) -> Result<Arc<Self>> {
        let node = Arc::new(Self {
            endpoint: Endpoint::bind(identity, addr)?,
            shares: RwLock::new(HashMap::new()),
        });
        tokio::spawn(server::run(node.clone()));
        Ok(node)
    }

    pub fn node_id(&self) -> NodeId {
        self.endpoint.node_id()
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        self.endpoint.local_addr()
    }

    pub(crate) fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Offers a complete artifact whose files live at `root`. Chunks are
    /// served straight from those files.
    pub fn seed(&self, artifact: Arc<Artifact>, root: PathBuf) -> Arc<Share> {
        let have = Bitfield::full(artifact.chunk_count());
        self.register(Arc::new(Share::new(DiskStore::new(artifact, root), have)))
    }

    /// Adds a share, or returns the one already registered for its artifact.
    pub(crate) fn register(&self, share: Arc<Share>) -> Arc<Share> {
        self.shares
            .write()
            .unwrap()
            .entry(share.artifact().id())
            .or_insert(share)
            .clone()
    }

    pub fn share(&self, artifact: Hash) -> Option<Arc<Share>> {
        self.shares.read().unwrap().get(&artifact).cloned()
    }

    /// A ticket naming this node as the peer to contact, reachable at each of
    /// `addrs` (`host:port`).
    pub fn ticket(&self, artifact: Hash, addrs: &[String]) -> Ticket {
        Ticket {
            artifact,
            peers: addrs
                .iter()
                .map(|addr| TicketPeer {
                    node: self.node_id(),
                    addr: addr.clone(),
                })
                .collect(),
        }
    }

    /// Closes all connections and stops serving.
    pub fn shutdown(&self) {
        self.endpoint.close();
    }
}
