use super::server;
use super::share::Share;
use super::{Ticket, TicketPeer, Whereabouts};
use crate::error::Result;
use crate::net::{Endpoint, Identity, NodeId, Options};
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
    /// Opens the node's sockets and starts answering peers.
    ///
    /// # Errors
    ///
    /// Returns an error if the address to listen on cannot be bound.
    pub async fn bind(identity: &Identity, options: &Options) -> Result<Arc<Self>> {
        let node = Arc::new(Self {
            endpoint: Endpoint::bind(identity, options).await?,
            shares: RwLock::new(HashMap::new()),
        });
        tokio::spawn(server::run(node.clone()));
        Ok(node)
    }

    /// This node's id.
    pub fn node_id(&self) -> NodeId {
        self.endpoint.node_id()
    }

    /// The addresses the node listens on.
    pub fn local_addrs(&self) -> Vec<SocketAddr> {
        self.endpoint.local_addrs()
    }

    /// Waits until the node has reached a relay, if it is to use one. From
    /// then on a ticket can say which.
    pub async fn online(&self) {
        self.endpoint.online().await;
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

    /// A ticket naming this node as the peer to ask: at each of `addrs`
    /// (`host:port`), and by way of its relay if it has reached one. With
    /// neither, the ticket names the node alone, to be looked up by its
    /// id.
    pub fn ticket(&self, artifact: Hash, addrs: &[String]) -> Ticket {
        let node = self.node_id();
        let addresses = addrs.iter().cloned().map(Whereabouts::Address);
        let relay = self.endpoint.relay().map(Whereabouts::Relay);
        let mut peers: Vec<TicketPeer> = addresses
            .chain(relay)
            .map(|at| TicketPeer { node, at: Some(at) })
            .collect();
        if peers.is_empty() {
            peers.push(TicketPeer { node, at: None });
        }
        Ticket { artifact, peers }
    }

    /// Closes all connections and stops serving. Returns once the other
    /// ends have been told, or have not answered for a while.
    pub async fn shutdown(&self) {
        self.endpoint.close().await;
    }
}
