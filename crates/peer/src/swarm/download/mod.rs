//! Downloading from a swarm.
//!
//! A download fetches the manifest from any bootstrap peer, checks what is
//! already on disk, then has the engine pull the missing chunks from as
//! many peers as it can find. Every chunk is verified against the manifest
//! before it is written, and is offered to other peers as soon as it lands.
//! A file gets its final name only when every one of its chunks is in
//! place.

mod supplier;

use self::supplier::Swarm;
use super::{Node, Share, Ticket, Whereabouts};
use crate::error::{Error, Result};
use crate::net::{NodeId, PeerInfo};
use engine::{Store, sync};
use fs::Folder;
use model::artifact::{Artifact, Hash};
use model::place::{Event, Observer};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::time::timeout;
use tracing::{debug, warn};

const MANIFEST_TIMEOUT: Duration = Duration::from_secs(15);
const MANIFEST_ATTEMPTS: usize = 3;
const ANNOUNCE_INTERVAL: Duration = Duration::from_secs(30);

/// Limits for one download.
#[derive(Debug, Clone)]
pub struct DownloadOptions {
    /// Peers to download from at the same time.
    pub max_peers: usize,
    /// Chunks requested in parallel from each peer.
    pub requests_per_peer: usize,
    /// Give up when no chunk has arrived for this long.
    pub stall_timeout: Duration,
}

impl Default for DownloadOptions {
    fn default() -> Self {
        Self {
            max_peers: 8,
            requests_per_peer: 4,
            stall_timeout: Duration::from_secs(60),
        }
    }
}

/// The stage a download is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Phase {
    /// Looking for a peer that can supply the manifest.
    Connecting = 0,
    /// Hashing files already on disk.
    Scanning = 1,
    /// Fetching chunks.
    Downloading = 2,
    /// Every chunk is on disk and verified, and every file has its name.
    Done = 3,
}

/// Live counters for a download, safe to read from another task.
#[derive(Debug, Default)]
pub struct Progress {
    phase: AtomicU8,
    total: AtomicU64,
    scanned: AtomicU64,
    resumed: AtomicU64,
    fetched: AtomicU64,
    peers: AtomicUsize,
}

impl Progress {
    /// The current stage.
    pub fn phase(&self) -> Phase {
        match self.phase.load(Ordering::Relaxed) {
            0 => Phase::Connecting,
            1 => Phase::Scanning,
            2 => Phase::Downloading,
            _ => Phase::Done,
        }
    }

    /// Size of the artifact; zero until the manifest has arrived.
    pub fn total_bytes(&self) -> u64 {
        self.total.load(Ordering::Relaxed)
    }

    /// Bytes examined so far while scanning existing files.
    pub fn scanned_bytes(&self) -> u64 {
        self.scanned.load(Ordering::Relaxed)
    }

    /// Bytes that were already on disk and valid.
    pub fn resumed_bytes(&self) -> u64 {
        self.resumed.load(Ordering::Relaxed)
    }

    /// Bytes fetched from peers and verified.
    pub fn fetched_bytes(&self) -> u64 {
        self.fetched.load(Ordering::Relaxed)
    }

    /// Bytes on disk and verified, whether resumed or fetched.
    pub fn done_bytes(&self) -> u64 {
        self.resumed_bytes() + self.fetched_bytes()
    }

    /// Peers currently connected.
    pub fn peers(&self) -> usize {
        self.peers.load(Ordering::Relaxed)
    }

    fn set_phase(&self, phase: Phase) {
        self.phase.store(phase as u8, Ordering::Relaxed);
    }

    /// Counts what the engine reports.
    fn observe(&self, event: Event) {
        match event {
            Event::Opened { .. } => {
                self.peers.fetch_add(1, Ordering::Relaxed);
            }
            Event::Closed { .. } => {
                self.peers.fetch_sub(1, Ordering::Relaxed);
            }
            Event::Stored { bytes, .. } => {
                self.fetched.fetch_add(bytes, Ordering::Relaxed);
            }
            Event::Committed { .. } | Event::Examined { .. } => {}
        }
    }
}

/// The result of a finished download.
#[derive(Debug)]
pub struct Report {
    /// The artifact, now registered with the node and served to others.
    pub share: Arc<Share>,
    /// The file, or the directory holding the tree, that was written.
    pub root: PathBuf,
    /// Bytes that were already on disk and valid.
    pub resumed_bytes: u64,
    /// Bytes fetched from peers.
    pub fetched_bytes: u64,
    /// Bytes fetched from each peer, largest first.
    pub sources: Vec<(NodeId, u64)>,
    /// Every peer this download learned about, for [`announce`].
    pub peers: Vec<PeerInfo>,
    /// Links of the artifact that were left out because this system cannot
    /// create them.
    pub skipped_links: usize,
    /// Wall time from start to finish.
    pub elapsed: Duration,
}

/// Downloads the ticket's artifact into `out_dir`, resuming from whatever is
/// already there.
///
/// # Errors
///
/// Returns [`Error::InvalidTicket`] if no peer in the ticket can be reached,
/// [`Error::ManifestUnavailable`] if none supplies a valid manifest, [`Error::Stalled`] if no
/// chunk arrives within the stall timeout, and [`Error::Place`] for local disk failures.
pub async fn download(
    node: &Arc<Node>,
    ticket: &Ticket,
    out_dir: &Path,
    options: DownloadOptions,
    progress: Arc<Progress>,
) -> Result<Report> {
    let started = Instant::now();
    progress.set_phase(Phase::Connecting);

    let bootstrap = resolve(node, ticket).await;
    if bootstrap.is_empty() {
        return Err(Error::InvalidTicket(
            "the ticket names no peer that can be reached".into(),
        ));
    }
    let artifact = Arc::new(fetch_manifest(node, ticket.artifact, &bootstrap).await?);
    progress
        .total
        .store(artifact.total_size(), Ordering::Relaxed);

    progress.set_phase(Phase::Scanning);
    let scan_progress = progress.clone();
    let folder = Arc::new(Folder::inside(artifact.tree().clone(), out_dir));
    let examined = move |bytes| {
        scan_progress.scanned.fetch_add(bytes, Ordering::Relaxed);
    };
    let store = Store::resume(artifact.clone(), folder.clone(), examined).await?;
    let have = store.held();

    let resumed: u64 = (0..artifact.chunk_count())
        .filter(|&index| have.get(index))
        .filter_map(|index| artifact.locate(index))
        .map(|location| u64::from(location.len))
        .sum();
    progress.resumed.store(resumed, Ordering::Relaxed);

    let share = node.register(Arc::new(Share::new(artifact.clone(), store)));

    progress.set_phase(Phase::Downloading);
    let named = bootstrap.iter().filter_map(|peer| peer.relay.clone());
    let swarm = Swarm {
        node: node.clone(),
        artifact,
        known: Arc::default(),
        relays: named.chain(node.endpoint().relay()).collect(),
    };
    let suppliers = bootstrap
        .into_iter()
        .map(|peer| swarm.supplier(peer))
        .collect();
    let limits = engine::Options {
        max_suppliers: options.max_peers,
        requests_per_supplier: options.requests_per_peer,
        stall_timeout: options.stall_timeout,
    };
    let watching = progress.clone();
    let observer: Observer = Arc::new(move |event| watching.observe(event));
    let me = *node.node_id().as_bytes();
    let summary = engine::transfer(share.store(), me, suppliers, limits, observer).await?;

    // Each file was flushed and renamed into place as its last chunk
    // arrived. What is left is to give the directories their times and
    // permissions, and to make the renames themselves last.
    share.store().finish().await?;
    progress.set_phase(Phase::Done);

    let known = sync::lock(&swarm.known);
    let peers = summary
        .usable
        .iter()
        .map(|id| NodeId::from_bytes(*id))
        .filter_map(|node| known.get(&node).cloned())
        .collect();
    Ok(Report {
        skipped_links: folder.skipped_links(),
        root: folder.root().to_owned(),
        share,
        resumed_bytes: resumed,
        fetched_bytes: progress.fetched_bytes(),
        sources: summary
            .sources
            .into_iter()
            .map(|(id, bytes)| (NodeId::from_bytes(id), bytes))
            .collect(),
        peers,
        elapsed: started.elapsed(),
    })
}

/// Keeps this node listed with `peers` as a holder of `content`, a content
/// id, so that downloaders arriving later are told about it. Runs until
/// cancelled.
pub async fn announce(node: Arc<Node>, content: Hash, peers: Vec<PeerInfo>) {
    let mut ticker = tokio::time::interval(ANNOUNCE_INTERVAL);
    loop {
        ticker.tick().await;
        for peer in &peers {
            let attempt = async {
                let connection = node.endpoint().connect(peer).await?;
                let status = connection.status(content).await;
                connection.close();
                status
            };
            match timeout(MANIFEST_TIMEOUT, attempt).await {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => debug!(peer = %peer.node.short(), "announce failed: {e}"),
                Err(_) => debug!(peer = %peer.node.short(), "announce timed out"),
            }
        }
    }
}

/// The peers a ticket names, each with every place the ticket gives for
/// it: its addresses, looked up if they are names, and its relay.
async fn resolve(node: &Node, ticket: &Ticket) -> Vec<PeerInfo> {
    let mut peers: Vec<PeerInfo> = Vec::new();
    for entry in &ticket.peers {
        if entry.node == node.node_id() {
            continue;
        }
        let known = peers.iter().position(|peer| peer.node == entry.node);
        let index = known.unwrap_or_else(|| {
            peers.push(PeerInfo::of(entry.node));
            peers.len() - 1
        });
        let peer = &mut peers[index];
        match &entry.at {
            None => {}
            Some(Whereabouts::Relay(url)) => peer.relay = Some(url.clone()),
            Some(Whereabouts::Address(addr)) => {
                match tokio::net::lookup_host(addr.as_str()).await {
                    Ok(addrs) => peer.addrs.extend(addrs),
                    Err(e) => warn!("cannot resolve {addr}: {e}"),
                }
            }
        }
    }
    peers
}

async fn fetch_manifest(node: &Node, id: Hash, peers: &[PeerInfo]) -> Result<Artifact> {
    for attempt in 0..MANIFEST_ATTEMPTS {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        for peer in peers {
            let request = async {
                let connection = node.endpoint().connect(peer).await?;
                let encoded = connection.manifest(id).await;
                connection.close();
                encoded
            };
            let encoded = match timeout(MANIFEST_TIMEOUT, request).await {
                Ok(Ok(Some(encoded))) => encoded,
                Ok(Ok(None)) => {
                    debug!(peer = %peer.node.short(), "peer does not serve this artifact");
                    continue;
                }
                Ok(Err(e)) => {
                    debug!(peer = %peer.node.short(), "manifest request failed: {e}");
                    continue;
                }
                Err(_) => {
                    debug!(peer = %peer.node.short(), "manifest request timed out");
                    continue;
                }
            };
            match Artifact::decode(id, encoded) {
                Ok(artifact) => return Ok(artifact),
                Err(e) => warn!(peer = %peer.node.short(), "rejected manifest: {e}"),
            }
        }
    }
    Err(Error::ManifestUnavailable)
}
