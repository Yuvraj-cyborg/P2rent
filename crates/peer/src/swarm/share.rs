use crate::net::{NodeId, PeerInfo};
use crate::{Error, Result};
use engine::{Store, sync};
use fs::Folder;
use model::access::{Guests, Proof, Secret};
use model::artifact::{Artifact, Bitfield};
use model::rolling::Rolling;
use model::search::ChunkHints;
use model::ticket::Ticket;
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

/// Who may fetch a share, and whether those who do pass it on.
#[derive(Debug, Clone)]
pub struct Access {
    /// What a machine must show it knows before it is answered. The
    /// share's ticket carries it.
    pub secret: Secret,
    /// Whether the machines that fetch the share give it to each other:
    /// see [`Ticket::swarm`].
    pub swarm: bool,
    /// Which of the machines that know the secret this node lets in. The
    /// rule is this node's alone: the others of a swarm have no way to know
    /// of it, and let in every machine that knows the secret. So a rule
    /// narrower than that means something only where there is no swarm.
    pub guests: Guests,
}

impl Access {
    /// For a new offer: a secret of its own, every machine that is given
    /// the ticket, and no swarm.
    pub fn new() -> Self {
        Self {
            secret: crate::net::random_secret(),
            swarm: false,
            guests: Guests::anyone(),
        }
    }

    /// What `ticket` says of the offer it names: for a machine that holds
    /// the same bytes and gives them to the holders of that ticket.
    pub fn of(ticket: &Ticket) -> Self {
        Self {
            secret: ticket.secret,
            swarm: ticket.swarm,
            guests: Guests::anyone(),
        }
    }
}

impl Default for Access {
    fn default() -> Self {
        Self::new()
    }
}

/// What a share says to a machine that asks for any of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Admission {
    /// It did not show that it knows the secret.
    Unproven,
    /// It knows the secret, and the share is for other machines.
    Refused,
    /// It may have what it asks for.
    Admitted,
}

/// One artifact this node holds, fully or partly, and offers to peers.
#[derive(Debug)]
pub struct Share {
    artifact: Arc<Artifact>,
    store: Arc<Store>,
    files: Arc<Folder>,
    secret: Secret,
    swarm: bool,
    guests: Mutex<Guests>,
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
    pub(crate) fn new(
        artifact: Arc<Artifact>,
        store: Arc<Store>,
        files: Arc<Folder>,
        access: Access,
    ) -> Self {
        Self {
            artifact,
            store,
            files,
            secret: access.secret,
            swarm: access.swarm,
            guests: Mutex::new(access.guests),
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

    /// What a machine must show it knows before it is answered.
    pub fn secret(&self) -> Secret {
        self.secret
    }

    /// Whether the machines that fetch the share give it to each other.
    pub fn swarm(&self) -> bool {
        self.swarm
    }

    /// Whether `peer`, which showed `proof` to this node, `me`, may have
    /// what it asks for.
    ///
    /// The secret is looked at first, so that only a machine that knows it
    /// can take one of the places there are.
    pub(crate) fn admit(&self, me: &NodeId, peer: &NodeId, proof: &Proof) -> Admission {
        if self.secret.proof(me, peer) != *proof {
            Admission::Unproven
        } else if sync::lock(&self.guests).admit(peer) {
            Admission::Admitted
        } else {
            Admission::Refused
        }
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

    /// Records that `peer` is taking part in this swarm, if the share is a
    /// swarm's. Otherwise nobody is told of anybody else, and nothing is
    /// kept.
    pub(crate) fn note_participant(&self, peer: PeerInfo) {
        if !self.swarm {
            return;
        }
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
    /// There are none unless the share is a swarm's.
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
