use crate::artifact::{Artifact, Bitfield};
use crate::net::{NodeId, PeerInfo};
use crate::store::DiskStore;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

/// A participant is forgotten if it has not asked for status for this long.
const PARTICIPANT_TTL: Duration = Duration::from_secs(120);

/// Most participants returned in one status reply.
const MAX_ADVERTISED_PEERS: usize = 32;

/// Most participants remembered per artifact.
const MAX_PARTICIPANTS: usize = 4096;

/// One artifact this node holds, fully or partly, and offers to peers.
#[derive(Debug)]
pub struct Share {
    store: DiskStore,
    have: RwLock<Bitfield>,
    participants: Mutex<HashMap<NodeId, Participant>>,
    uploaded: AtomicU64,
}

#[derive(Debug, Clone, Copy)]
struct Participant {
    addr: std::net::SocketAddr,
    seen: Instant,
}

impl Share {
    pub(crate) fn new(store: DiskStore, have: Bitfield) -> Self {
        Self {
            store,
            have: RwLock::new(have),
            participants: Mutex::new(HashMap::new()),
            uploaded: AtomicU64::new(0),
        }
    }

    pub fn artifact(&self) -> &Arc<Artifact> {
        self.store.artifact()
    }

    pub fn store(&self) -> &DiskStore {
        &self.store
    }

    pub fn has(&self, index: u64) -> bool {
        self.have.read().unwrap().get(index)
    }

    pub fn have(&self) -> Bitfield {
        self.have.read().unwrap().clone()
    }

    pub fn is_complete(&self) -> bool {
        self.have.read().unwrap().is_full()
    }

    /// Bytes served to other peers so far.
    pub fn uploaded_bytes(&self) -> u64 {
        self.uploaded.load(Ordering::Relaxed)
    }

    pub(crate) fn mark(&self, index: u64) {
        self.have.write().unwrap().set(index);
    }

    pub(crate) fn add_uploaded(&self, bytes: u64) {
        self.uploaded.fetch_add(bytes, Ordering::Relaxed);
    }

    /// Records that `peer` is taking part in this swarm.
    pub(crate) fn note_participant(&self, peer: PeerInfo) {
        let now = Instant::now();
        let mut participants = self.participants.lock().unwrap();
        if participants.len() >= MAX_PARTICIPANTS && !participants.contains_key(&peer.node) {
            participants.retain(|_, p| now.duration_since(p.seen) < PARTICIPANT_TTL);
            if participants.len() >= MAX_PARTICIPANTS {
                return;
            }
        }
        participants.insert(
            peer.node,
            Participant {
                addr: peer.addr,
                seen: now,
            },
        );
    }

    /// Recently seen participants other than `except`, most recent first.
    pub(crate) fn participants(&self, except: NodeId) -> Vec<PeerInfo> {
        let now = Instant::now();
        let mut participants = self.participants.lock().unwrap();
        participants.retain(|_, p| now.duration_since(p.seen) < PARTICIPANT_TTL);
        let mut recent: Vec<(Instant, PeerInfo)> = participants
            .iter()
            .filter(|(node, _)| **node != except)
            .map(|(&node, p)| (p.seen, PeerInfo { node, addr: p.addr }))
            .collect();
        recent.sort_by_key(|entry| std::cmp::Reverse(entry.0));
        recent.truncate(MAX_ADVERTISED_PEERS);
        recent.into_iter().map(|(_, peer)| peer).collect()
    }
}
