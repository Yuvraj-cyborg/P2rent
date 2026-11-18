use crate::net::{NodeId, PeerInfo};
use crate::{Error, Result};
use engine::{Store, sync};
use fs::Folder;
use model::artifact::{Artifact, Bitfield};
use model::rolling::Rolling;
use model::search::ChunkHints;
use std::collections::HashMap;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::OnceCell;

/// A participant is forgotten if it has not asked for status for this long.
const PARTICIPANT_TTL: Duration = Duration::from_secs(120);

/// Most participants returned in one status reply.
const MAX_ADVERTISED_PEERS: usize = 32;

/// Most participants remembered per artifact.
const MAX_PARTICIPANTS: usize = 4096;

/// One artifact this node holds, fully or partly, and offers to peers.
#[derive(Debug)]
pub struct Share {
    artifact: Arc<Artifact>,
    store: Arc<Store>,
    files: Arc<Folder>,
    /// Allocated on the first hint request. Concurrent readers share each
    /// completed page. A cancelled response can resume with cached pages.
    hints: OnceLock<Vec<OnceCell<ChunkHints>>>,
    participants: Mutex<HashMap<NodeId, Participant>>,
    uploaded: AtomicU64,
}

#[derive(Debug, Clone)]
struct Participant {
    /// The peer, and where it was when it last asked.
    peer: PeerInfo,
    seen: Instant,
}

impl Share {
    pub(crate) fn new(artifact: Arc<Artifact>, store: Arc<Store>, files: Arc<Folder>) -> Self {
        Self {
            artifact,
            store,
            files,
            hints: OnceLock::new(),
            participants: Mutex::new(HashMap::new()),
            uploaded: AtomicU64::new(0),
        }
    }

    /// The artifact being shared.
    pub fn artifact(&self) -> &Arc<Artifact> {
        &self.artifact
    }

    /// Where its chunks are kept.
    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }

    /// Whether chunk `index` is held.
    pub fn has(&self, index: u64) -> bool {
        self.store.has(index)
    }

    /// A snapshot of which chunks are held.
    pub fn have(&self) -> Bitfield {
        self.store.held()
    }

    /// Whether every chunk is held.
    pub fn is_complete(&self) -> bool {
        self.store.is_complete()
    }

    /// Number of pages in the table for this share.
    pub(super) fn hint_pages(&self) -> u64 {
        self.artifact
            .chunk_count()
            .div_ceil(u64::from(crate::net::proto::hint_page_chunks(
                self.artifact.chunk_size(),
            )))
    }

    /// Computes each page on first use and shares it across receivers.
    pub(super) async fn hint_page(&self, page: u64) -> Result<&ChunkHints> {
        let pages = self
            .hints
            .get_or_init(|| (0..self.hint_pages()).map(|_| OnceCell::new()).collect());
        let cell = usize::try_from(page)
            .ok()
            .and_then(|i| pages.get(i))
            .ok_or_else(|| Error::Protocol("hint page out of range".into()))?;
        cell.get_or_try_init(|| async {
            let per_page = u64::from(crate::net::proto::hint_page_chunks(
                self.artifact.chunk_size(),
            ));
            let first = page * per_page;
            let end = (first + per_page).min(self.artifact.chunk_count());
            let files = self.files.clone();
            let artifact = self.artifact.clone();
            tokio::task::spawn_blocking(move || {
                let hashes = (first..end)
                    .map(|index| {
                        let at = artifact
                            .locate(index)
                            .ok_or_else(|| Error::Protocol("chunk outside manifest".into()))?;
                        let data = files.read_at(at.file, at.offset, at.len)?;
                        Ok(Rolling::of(&data).value())
                    })
                    .collect::<Result<Vec<_>>>()?;
                Ok(ChunkHints::pack(hashes.into_iter()))
            })
            .await
            .map_err(|error| Error::Transport(error.to_string()))?
        })
        .await
    }

    /// Bytes served to other peers so far.
    pub fn uploaded_bytes(&self) -> u64 {
        self.uploaded.load(Ordering::Relaxed)
    }

    pub(crate) fn add_uploaded(&self, bytes: u64) {
        self.uploaded.fetch_add(bytes, Ordering::Relaxed);
    }

    /// Records that `peer` is taking part in this swarm.
    pub(crate) fn note_participant(&self, peer: PeerInfo) {
        let now = Instant::now();
        let mut participants = sync::lock(&self.participants);
        if participants.len() >= MAX_PARTICIPANTS && !participants.contains_key(&peer.node) {
            participants.retain(|_, p| now.duration_since(p.seen) < PARTICIPANT_TTL);
            if participants.len() >= MAX_PARTICIPANTS {
                return;
            }
        }
        participants.insert(peer.node, Participant { peer, seen: now });
    }

    /// Recently seen participants other than `except`, most recent first.
    pub(crate) fn participants(&self, except: NodeId) -> Vec<PeerInfo> {
        let now = Instant::now();
        let mut participants = sync::lock(&self.participants);
        participants.retain(|_, p| now.duration_since(p.seen) < PARTICIPANT_TTL);
        let mut recent: Vec<(Instant, PeerInfo)> = participants
            .iter()
            .filter(|(node, _)| **node != except)
            .map(|(_, p)| (p.seen, p.peer.clone()))
            .collect();
        recent.sort_by_key(|entry| std::cmp::Reverse(entry.0));
        recent.truncate(MAX_ADVERTISED_PEERS);
        recent.into_iter().map(|(_, peer)| peer).collect()
    }
}
