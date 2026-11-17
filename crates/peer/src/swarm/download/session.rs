use super::claims::{Claim, Claims, Shared};
use super::{DownloadOptions, Progress};
use crate::net::{NodeId, PeerInfo};
use crate::swarm::{Node, Share};
use crate::sync;
use model::artifact::{Artifact, Bitfield};
use model::schedule::Owner;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::{Notify, mpsc};

/// State shared by every task working on one download.
pub(super) struct Session {
    pub node: Arc<Node>,
    pub share: Arc<Share>,
    pub artifact: Arc<Artifact>,
    pub options: DownloadOptions,
    pub progress: Arc<Progress>,
    /// Woken when a chunk is released or the download completes.
    pub wake: Arc<Notify>,
    claims: Shared,
    /// The number the next connection fetches under.
    owners: AtomicU32,
    discovered: mpsc::UnboundedSender<PeerInfo>,
    fetched_from: Mutex<HashMap<NodeId, u64>>,
}

impl Session {
    pub(super) fn new(
        node: Arc<Node>,
        share: Arc<Share>,
        options: DownloadOptions,
        progress: Arc<Progress>,
        discovered: mpsc::UnboundedSender<PeerInfo>,
    ) -> Arc<Self> {
        let wake = Arc::new(Notify::new());
        let claims = Claims::new(&share.have(), {
            let wake = wake.clone();
            move || wake.notify_waiters()
        });
        Arc::new(Self {
            node,
            artifact: share.artifact().clone(),
            share,
            options,
            progress,
            wake,
            claims,
            owners: AtomicU32::new(0),
            discovered,
            fetched_from: Mutex::new(HashMap::new()),
        })
    }

    pub(super) fn is_complete(&self) -> bool {
        self.claims.is_complete()
    }

    /// When a chunk was last completed, or when the download started.
    pub(super) fn last_progress(&self) -> Instant {
        self.claims.last_progress()
    }

    /// A number for one connection to fetch under, so that the schedule
    /// knows whose each chunk in flight is.
    pub(super) fn new_owner(&self) -> Owner {
        self.owners.fetch_add(1, Ordering::Relaxed)
    }

    /// Reserves for `owner` a missing chunk that the peer holds, scanning
    /// from `cursor`.
    pub(super) fn claim(
        &self,
        owner: Owner,
        peer_has: &Bitfield,
        cursor: &mut u64,
    ) -> Option<Claim> {
        Claims::claim(&self.claims, owner, peer_has, cursor)
    }

    /// Records a chunk that the store has verified and written to disk.
    pub(super) fn complete(&self, claim: Claim, from: NodeId, bytes: u64) {
        self.progress.add_fetched(bytes);
        *sync::lock(&self.fetched_from).entry(from).or_default() += bytes;
        self.claims.complete(claim);
    }

    /// Reports a peer learned from another peer.
    pub(super) fn discover(&self, peer: PeerInfo) {
        let _ = self.discovered.send(peer);
    }

    pub(super) fn fetched_from(&self) -> Vec<(NodeId, u64)> {
        let mut sources: Vec<_> = sync::lock(&self.fetched_from)
            .iter()
            .map(|(&node, &bytes)| (node, bytes))
            .collect();
        sources.sort_by_key(|source| std::cmp::Reverse(source.1));
        sources
    }
}
