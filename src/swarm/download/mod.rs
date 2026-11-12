//! Multi-source download.
//!
//! A download fetches the manifest from any bootstrap peer, checks what is
//! already on disk, then pulls the missing chunks from as many peers as it
//! can find. Every chunk is verified against the manifest before it is
//! written, and is offered to other peers as soon as it lands.

mod peer;
mod session;

use self::peer::Outcome;
use self::session::Session;
use super::{Node, Share, Ticket};
use crate::artifact::{Artifact, Hash};
use crate::error::{Error, Result};
use crate::net::{NodeId, PeerInfo};
use crate::store::DiskStore;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio::time::timeout;
use tracing::{debug, warn};

const MANIFEST_TIMEOUT: Duration = Duration::from_secs(15);
const MANIFEST_ATTEMPTS: usize = 3;
const MANAGER_TICK: Duration = Duration::from_millis(250);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);
const ANNOUNCE_INTERVAL: Duration = Duration::from_secs(30);

/// Most peers a download will keep track of.
const MAX_KNOWN_PEERS: usize = 512;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Phase {
    /// Looking for a peer that can supply the manifest.
    Connecting = 0,
    /// Hashing files already on disk.
    Scanning = 1,
    /// Fetching chunks.
    Downloading = 2,
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
    pub(crate) peers: AtomicUsize,
}

impl Progress {
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

    pub(crate) fn add_fetched(&self, bytes: u64) {
        self.fetched.fetch_add(bytes, Ordering::Relaxed);
    }
}

/// The result of a finished download.
#[derive(Debug)]
pub struct Report {
    /// The artifact, now registered with the node and served to others.
    pub share: Arc<Share>,
    pub resumed_bytes: u64,
    pub fetched_bytes: u64,
    /// Bytes fetched from each peer, largest first.
    pub sources: Vec<(NodeId, u64)>,
    /// Every peer this download learned about, for [`announce`].
    pub peers: Vec<PeerInfo>,
    pub elapsed: Duration,
}

/// Downloads the ticket's artifact into `out_dir`, resuming from whatever is
/// already there.
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
    let store = DiskStore::inside(artifact.clone(), out_dir);
    let scan_progress = progress.clone();
    let (store, have) = tokio::task::spawn_blocking(move || {
        let have = store.scan(&|bytes| {
            scan_progress.scanned.fetch_add(bytes, Ordering::Relaxed);
        })?;
        store.allocate()?;
        Ok::<_, std::io::Error>((store, have))
    })
    .await
    .map_err(|e| Error::Transport(e.to_string()))??;

    let resumed: u64 = (0..artifact.chunk_count())
        .filter(|&index| have.get(index))
        .filter_map(|index| artifact.locate(index))
        .map(|location| u64::from(location.len))
        .sum();
    progress.resumed.store(resumed, Ordering::Relaxed);

    let share = node.register(Arc::new(Share::new(store, have)));
    progress.set_phase(Phase::Downloading);

    let (discovered_tx, discovered_rx) = mpsc::unbounded_channel();
    let session = Session::new(
        node.clone(),
        share.clone(),
        options,
        progress.clone(),
        discovered_tx,
    );
    let peers = manage(&session, bootstrap, discovered_rx).await?;

    let finalize = share.clone();
    tokio::task::spawn_blocking(move || finalize.store().finalize())
        .await
        .map_err(|e| Error::Transport(e.to_string()))??;
    progress.set_phase(Phase::Done);

    Ok(Report {
        share,
        resumed_bytes: resumed,
        fetched_bytes: progress.fetched_bytes(),
        sources: session.fetched_from(),
        peers,
        elapsed: started.elapsed(),
    })
}

/// Keeps this node listed with `peers` as a holder of `artifact`, so that
/// downloaders arriving later are told about it. Runs until cancelled.
pub async fn announce(node: Arc<Node>, artifact: Hash, peers: Vec<PeerInfo>) {
    let mut ticker = tokio::time::interval(ANNOUNCE_INTERVAL);
    loop {
        ticker.tick().await;
        for peer in &peers {
            let attempt = async {
                let connection = node.endpoint().connect(peer.addr, peer.node).await?;
                let status = connection.status(artifact).await;
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

/// Turns the ticket's `host:port` entries into socket addresses.
async fn resolve(node: &Node, ticket: &Ticket) -> Vec<PeerInfo> {
    let want_v4 = node.local_addr().map(|a| a.is_ipv4()).unwrap_or(true);
    let mut peers = Vec::new();
    for entry in &ticket.peers {
        if entry.node == node.node_id() {
            continue;
        }
        let resolved: Vec<SocketAddr> = match tokio::net::lookup_host(entry.addr.as_str()).await {
            Ok(addrs) => addrs.collect(),
            Err(e) => {
                warn!("cannot resolve {}: {e}", entry.addr);
                continue;
            }
        };
        // Prefer an address the local socket can actually dial.
        let addr = resolved
            .iter()
            .find(|addr| addr.is_ipv4() == want_v4)
            .or(resolved.first());
        if let Some(&addr) = addr {
            peers.push(PeerInfo {
                node: entry.node,
                addr,
            });
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
                let connection = node.endpoint().connect(peer.addr, peer.node).await?;
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

struct Slot {
    addr: SocketAddr,
    active: bool,
    banned: bool,
    failures: u32,
    retry_at: Instant,
}

impl Slot {
    fn new(addr: SocketAddr) -> Self {
        Self {
            addr,
            active: false,
            banned: false,
            failures: 0,
            retry_at: Instant::now(),
        }
    }
}

/// Runs the download to completion: starts a task per peer, feeds it newly
/// discovered peers, retries the ones that fail and drops the ones that lie.
/// Returns every usable peer it came to know.
async fn manage(
    session: &Arc<Session>,
    bootstrap: Vec<PeerInfo>,
    mut discovered: mpsc::UnboundedReceiver<PeerInfo>,
) -> Result<Vec<PeerInfo>> {
    let own = session.node.node_id();
    let mut slots: HashMap<NodeId, Slot> = bootstrap
        .into_iter()
        .map(|peer| (peer.node, Slot::new(peer.addr)))
        .collect();
    let mut tasks: JoinSet<Outcome> = JoinSet::new();
    let mut running: HashMap<tokio::task::Id, NodeId> = HashMap::new();
    let mut ticker = tokio::time::interval(MANAGER_TICK);

    while !session.is_complete() {
        let now = Instant::now();
        let mut free = session
            .options
            .max_peers
            .max(1)
            .saturating_sub(running.len());
        for (&node, slot) in slots.iter_mut() {
            if free == 0 {
                break;
            }
            if slot.active || slot.banned || slot.retry_at > now {
                continue;
            }
            slot.active = true;
            let peer = PeerInfo {
                node,
                addr: slot.addr,
            };
            let handle = tasks.spawn(peer::run(session.clone(), peer));
            running.insert(handle.id(), node);
            free -= 1;
        }

        tokio::select! {
            Some(peer) = discovered.recv() => {
                let known = slots.len();
                if peer.node == own {
                    // Peers may list us; there is nothing to do.
                } else if let Some(slot) = slots.get_mut(&peer.node) {
                    if !slot.active {
                        slot.addr = peer.addr;
                    }
                } else if known < MAX_KNOWN_PEERS {
                    debug!(peer = %peer.node.short(), addr = %peer.addr, "discovered peer");
                    slots.insert(peer.node, Slot::new(peer.addr));
                }
            }
            Some(joined) = tasks.join_next_with_id() => {
                let (id, outcome) = match joined {
                    Ok((id, outcome)) => (id, outcome),
                    Err(e) => (e.id(), Outcome::Failed(format!("peer task crashed: {e}"))),
                };
                let Some(node) = running.remove(&id) else { continue };
                let Some(slot) = slots.get_mut(&node) else { continue };
                slot.active = false;
                match outcome {
                    Outcome::Finished => {}
                    Outcome::Failed(reason) => {
                        slot.failures += 1;
                        let delay = Duration::from_secs(1 << slot.failures.min(5)).min(MAX_RETRY_DELAY);
                        slot.retry_at = Instant::now() + delay;
                        debug!(peer = %node.short(), "peer failed ({reason}); retrying in {delay:?}");
                    }
                    Outcome::Banned(reason) => {
                        slot.banned = true;
                        warn!(peer = %node.short(), "dropping peer: {reason}");
                    }
                    Outcome::Fatal(error) => return Err(error),
                }
            }
            _ = ticker.tick() => {}
            _ = session.wake.notified() => {}
        }

        let idle = session.last_progress().elapsed();
        if idle > session.options.stall_timeout {
            return Err(Error::Stalled(session.options.stall_timeout));
        }
    }

    tasks.shutdown().await;
    Ok(slots
        .into_iter()
        .filter(|(_, slot)| !slot.banned)
        .map(|(node, slot)| PeerInfo {
            node,
            addr: slot.addr,
        })
        .collect())
}
