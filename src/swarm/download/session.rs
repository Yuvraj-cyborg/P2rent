use super::{DownloadOptions, Progress};
use crate::artifact::{Artifact, Bitfield};
use crate::net::{NodeId, PeerInfo};
use crate::swarm::{Node, Share};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::{Notify, mpsc};

#[derive(Clone, Copy, PartialEq, Eq)]
enum ChunkState {
    Missing,
    InFlight,
    Done,
}

struct Scheduler {
    chunks: Vec<ChunkState>,
    remaining: u64,
    last_progress: Instant,
}

/// State shared by every task working on one download.
pub(super) struct Session {
    pub node: Arc<Node>,
    pub share: Arc<Share>,
    pub artifact: Arc<Artifact>,
    pub options: DownloadOptions,
    pub progress: Arc<Progress>,
    /// Woken when a chunk is released or the download completes.
    pub wake: Notify,
    scheduler: Mutex<Scheduler>,
    discovered: mpsc::UnboundedSender<PeerInfo>,
    fetched_from: Mutex<HashMap<NodeId, u64>>,
}

impl Session {
    pub fn new(
        node: Arc<Node>,
        share: Arc<Share>,
        options: DownloadOptions,
        progress: Arc<Progress>,
        discovered: mpsc::UnboundedSender<PeerInfo>,
    ) -> Arc<Self> {
        let have = share.have();
        let chunks: Vec<ChunkState> = (0..have.len())
            .map(|i| match have.get(i) {
                true => ChunkState::Done,
                false => ChunkState::Missing,
            })
            .collect();
        let remaining = have.len() - have.count_ones();
        Arc::new(Self {
            node,
            artifact: share.artifact().clone(),
            share,
            options,
            progress,
            wake: Notify::new(),
            scheduler: Mutex::new(Scheduler {
                chunks,
                remaining,
                last_progress: Instant::now(),
            }),
            discovered,
            fetched_from: Mutex::new(HashMap::new()),
        })
    }

    pub fn is_complete(&self) -> bool {
        self.scheduler.lock().unwrap().remaining == 0
    }

    /// When a chunk was last completed, or when the download started.
    pub fn last_progress(&self) -> Instant {
        self.scheduler.lock().unwrap().last_progress
    }

    /// Reserves a missing chunk that the peer holds, scanning from `cursor`.
    ///
    /// Each worker starts from a different offset, so concurrent downloaders
    /// collect different parts of the artifact first and can then trade them.
    pub fn claim(self: &Arc<Self>, peer_has: &Bitfield, cursor: &mut u64) -> Option<Claim> {
        let mut scheduler = self.scheduler.lock().unwrap();
        let total = scheduler.chunks.len() as u64;
        if scheduler.remaining == 0 {
            return None;
        }
        for step in 0..total {
            let index = (*cursor + step) % total;
            if scheduler.chunks[index as usize] == ChunkState::Missing && peer_has.get(index) {
                scheduler.chunks[index as usize] = ChunkState::InFlight;
                *cursor = (index + 1) % total;
                return Some(Claim {
                    session: self.clone(),
                    index,
                    settled: false,
                });
            }
        }
        None
    }

    /// Records a chunk that has been verified and written to disk.
    pub fn complete(&self, mut claim: Claim, from: NodeId, bytes: u64) {
        claim.settled = true;
        self.share.mark(claim.index);
        self.progress.add_fetched(bytes);
        *self.fetched_from.lock().unwrap().entry(from).or_default() += bytes;

        let mut scheduler = self.scheduler.lock().unwrap();
        scheduler.chunks[claim.index as usize] = ChunkState::Done;
        scheduler.remaining -= 1;
        scheduler.last_progress = Instant::now();
        if scheduler.remaining == 0 {
            self.wake.notify_waiters();
        }
    }

    fn release(&self, index: u64) {
        self.scheduler.lock().unwrap().chunks[index as usize] = ChunkState::Missing;
        self.wake.notify_waiters();
    }

    /// Reports a peer learned from another peer.
    pub fn discover(&self, peer: PeerInfo) {
        let _ = self.discovered.send(peer);
    }

    pub fn fetched_from(&self) -> Vec<(NodeId, u64)> {
        let mut sources: Vec<_> = self
            .fetched_from
            .lock()
            .unwrap()
            .iter()
            .map(|(&node, &bytes)| (node, bytes))
            .collect();
        sources.sort_by_key(|source| std::cmp::Reverse(source.1));
        sources
    }
}

/// A reserved chunk. Dropping it without completing puts the chunk back, so
/// a cancelled or failed request never strands a chunk.
pub(super) struct Claim {
    session: Arc<Session>,
    pub index: u64,
    settled: bool,
}

impl Drop for Claim {
    fn drop(&mut self) {
        if !self.settled {
            self.session.release(self.index);
        }
    }
}
