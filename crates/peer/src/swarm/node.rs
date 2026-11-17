use super::server;
use super::share::Share;
use super::{Ticket, TicketPeer};
use crate::error::Result;
use crate::net::{Endpoint, Identity, NodeId};
use engine::{Store, sync};
use fs::Folder;
use model::artifact::{Artifact, Hash};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

/// A participant in one or more swarms: one socket, one identity, and the
/// set of artifacts it currently offers.
#[derive(Debug)]
pub struct Node {
    endpoint: Endpoint,
    /// By content id, which is what a swarm exchanges chunks under.
    shares: RwLock<HashMap<Hash, Arc<Share>>>,
}

impl Node {
    /// Binds `addr` and starts answering peers. Must be called from within a
    /// Tokio runtime.
    ///
    /// # Errors
    ///
    /// Returns an error if the endpoint cannot be created or `addr` cannot be bound.
    pub fn bind(identity: &Identity, addr: SocketAddr) -> Result<Arc<Self>> {
        let node = Arc::new(Self {
            endpoint: Endpoint::bind(identity, addr)?,
            shares: RwLock::new(HashMap::new()),
        });
        tokio::spawn(server::run(node.clone()));
        Ok(node)
    }

    /// This node's id.
    pub fn node_id(&self) -> NodeId {
        self.endpoint.node_id()
    }

    /// The address the node listens on.
    ///
    /// # Errors
    ///
    /// Returns an error if the operating system cannot report the address.
    pub fn local_addr(&self) -> Result<SocketAddr> {
        self.endpoint.local_addr()
    }

    pub(crate) fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Offers a complete artifact whose files live at `root`. Chunks are
    /// served straight from those files.
    pub fn seed(&self, artifact: Arc<Artifact>, root: PathBuf) -> Arc<Share> {
        let files = Arc::new(Folder::whole(artifact.tree().clone(), root));
        let store = Store::whole(artifact.clone(), files);
        self.register(Arc::new(Share::new(artifact, store)))
    }

    /// Adds a share, or returns the one already registered for its content.
    pub(crate) fn register(&self, share: Arc<Share>) -> Arc<Share> {
        sync::write(&self.shares)
            .entry(share.artifact().content_id())
            .or_insert(share)
            .clone()
    }

    /// The share that holds `content`, if any.
    pub fn share(&self, content: Hash) -> Option<Arc<Share>> {
        sync::read(&self.shares).get(&content).cloned()
    }

    /// The share whose manifest has the id `artifact`, if any. A node can
    /// hold an artifact's content and still not have this manifest, when its
    /// own copy has other names, permissions or times.
    pub(crate) fn share_of_artifact(&self, artifact: Hash) -> Option<Arc<Share>> {
        sync::read(&self.shares)
            .values()
            .find(|share| share.artifact().id() == artifact)
            .cloned()
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
