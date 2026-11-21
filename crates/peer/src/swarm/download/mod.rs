//! Downloading from a swarm.
//!
//! A download finds what a ticket names: it fetches the manifest from any
//! peer the ticket gives. Then it fetches that: it checks what is already
//! on disk, and has the engine pull the missing chunks from as many peers
//! as it can find. Every chunk is verified against the manifest
//! before it is written. A file gets its final name only when every one of
//! its chunks is in place.
//!
//! Every request shows that this machine knows the ticket's secret. If the
//! ticket is a swarm's, each chunk is offered to the other holders of the
//! ticket as soon as it lands, and they are fetched from as they are heard
//! of. Otherwise this machine serves nothing, is listed nowhere, and
//! fetches from the peers of the ticket alone.

mod selection;
mod supplier;

use self::supplier::Swarm;
use super::{Access, Node, Share, Ticket, Whereabouts};
use crate::error::{Error, Result};
use crate::net::{NodeId, PeerInfo};
use engine::{Store, sync};
use fs::Folder;
use model::access::Secret;
use model::artifact::{Artifact, Bitfield};
use model::place::{Event, Observer, Supplier, SupplierId};
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
    /// Give up when no chunk has arrived for this long.
    pub stall_timeout: Duration,
}

impl Default for DownloadOptions {
    fn default() -> Self {
        Self {
            max_peers: 8,
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
            Event::Committed { .. }
            | Event::Examined { .. }
            | Event::Planned { .. }
            | Event::Kept { .. } => {}
        }
    }
}

/// The result of a finished download.
#[derive(Debug)]
pub struct Report {
    /// The artifact as this node now holds it. If the ticket was a swarm's
    /// it is registered with the node, and served to the others of the
    /// swarm.
    pub share: Arc<Share>,
    /// The file, or the directory holding the tree, that was written.
    pub root: PathBuf,
    /// Bytes that were already on disk and valid.
    pub resumed_bytes: u64,
    /// Bytes fetched.
    pub fetched_bytes: u64,
    /// Bytes fetched from each peer, largest first.
    pub sources: Vec<(NodeId, u64)>,
    /// Bytes fetched from each supplier that is not a peer and was added
    /// with [`Found::also`], largest first.
    pub others: Vec<(SupplierId, u64)>,
    /// Every peer this download learned about, for [`announce`].
    pub peers: Vec<PeerInfo>,
    /// Links of the artifact that were left out because this system cannot
    /// create them.
    pub skipped_links: usize,
    /// Wall time from start to finish.
    pub elapsed: Duration,
}

/// What a ticket names, once a peer has said: the artifact, and the peers
/// of the ticket with where each is.
#[derive(Debug, Clone)]
pub struct Found {
    /// The artifact, with the digest of every chunk.
    pub artifact: Arc<Artifact>,
    /// What the ticket says of who may fetch it, and whether they pass it
    /// on: the ticket's secret, and whether it is a swarm's.
    access: Access,
    /// The peers the ticket names, other than this node.
    peers: Vec<PeerInfo>,
    /// What else its chunks are to be fetched from.
    others: Vec<Arc<dyn Supplier>>,
    /// A peer already known to be reachable and to serve this manifest.
    manifest_peer: PeerInfo,
}

impl Found {
    /// Adds a supplier that is not a peer to fetch the chunks from as
    /// well: a web server that holds the same file, say. It is trusted no
    /// more than a peer is: what it sends is compared with the digests of
    /// the artifact, and it is dropped if it sends anything else.
    pub fn also(&mut self, supplier: Arc<dyn Supplier>) {
        self.others.push(supplier);
    }

    /// Whether the ticket is a swarm's: its holders fetch from each other.
    pub fn swarm(&self) -> bool {
        self.access.swarm
    }

    /// The peers to fetch from. `serving` says that this node serves what
    /// it fetches: in a swarm it then asks to be listed with the others.
    fn suppliers(&self, node: &Arc<Node>, serving: bool) -> (Swarm, Vec<Arc<dyn Supplier>>) {
        let named = self.peers.iter().filter_map(|peer| peer.relay.clone());
        let swarm = Swarm {
            node: node.clone(),
            artifact: self.artifact.clone(),
            secret: self.access.secret,
            follow: self.access.swarm,
            listed: self.access.swarm && serving,
            known: Arc::default(),
            relays: named.chain(node.endpoint().relay()).collect(),
        };
        let suppliers = self
            .peers
            .iter()
            .cloned()
            .map(|peer| swarm.supplier(peer))
            .chain(self.others.iter().cloned())
            .collect();
        (swarm, suppliers)
    }
}

/// One peer of a ticket, and how much of the artifact it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holder {
    /// The peer.
    pub node: NodeId,
    /// How many bytes of the artifact it holds, or `None` if it could not
    /// be asked or does not serve the artifact.
    pub bytes: Option<u64>,
}

/// Downloads the ticket's artifact into `out_dir`, resuming from whatever is
/// already there.
///
/// # Errors
///
/// Returns [`Error::InvalidTicket`] if no peer in the ticket can be reached,
/// [`Error::ManifestUnavailable`] if none supplies a valid manifest, [`Error::Refused`] if a
/// peer does not let this machine fetch, [`Error::Stalled`] if no chunk arrives within the
/// stall timeout, and [`Error::Place`] for local disk failures.
pub async fn download(
    node: &Arc<Node>,
    ticket: &Ticket,
    out_dir: &Path,
    options: DownloadOptions,
    progress: Arc<Progress>,
) -> Result<Report> {
    progress.set_phase(Phase::Connecting);
    let found = find(node, ticket).await?;
    fetch(node, found, out_dir, options, progress, Arc::new(|_| {})).await
}

/// Finds what a ticket names: asks the peers it gives for the manifest,
/// and checks what comes back against the ticket.
///
/// # Errors
///
/// Returns [`Error::InvalidTicket`] if no peer in the ticket can be reached,
/// [`Error::ManifestUnavailable`] if none supplies a valid manifest, and
/// [`Error::Refused`] if none does and one of them does not let this machine
/// fetch.
pub async fn find(node: &Arc<Node>, ticket: &Ticket) -> Result<Found> {
    let peers = resolve(node, ticket).await;
    if peers.is_empty() {
        return Err(Error::InvalidTicket(
            "the ticket names no peer that can be reached".into(),
        ));
    }
    let (artifact, manifest_peer) = fetch_manifest(node, ticket, &peers).await?;
    Ok(Found {
        artifact: Arc::new(artifact),
        access: Access::of(ticket),
        manifest_peer,
        peers,
        others: Vec::new(),
    })
}

/// Asks each peer of the ticket how much of the artifact it holds. A peer
/// that is itself still fetching holds a part.
pub async fn holders(node: &Arc<Node>, found: &Found) -> Vec<Holder> {
    let artifact = &found.artifact;
    let secret = found.access.secret;
    // All of them at once: peers that do not answer are waited for
    // together, and not one after the other.
    let asked: Vec<_> = found
        .peers
        .iter()
        .map(|peer| {
            let (node, peer, content) = (node.clone(), peer.clone(), artifact.content_id());
            tokio::spawn(async move {
                let asking = async {
                    let connection = node.endpoint().connect(&peer).await?;
                    // Asking serves nothing, so it asks to be listed nowhere.
                    let status = connection.status(content, &secret, false).await;
                    connection.close();
                    status
                };
                timeout(MANIFEST_TIMEOUT, asking).await
            })
        })
        .collect();
    let mut holders = Vec::with_capacity(asked.len());
    for (peer, asked) in found.peers.iter().zip(asked) {
        let held = match asked.await {
            Ok(Ok(Ok(Some(status)))) => status.have.into_bitfield(artifact.chunk_count()).ok(),
            _ => None,
        };
        holders.push(Holder {
            node: peer.node,
            bytes: held.map(|held| bytes_of(artifact, &held)),
        });
    }
    holders
}

/// How many bytes the chunks in `held` come to.
fn bytes_of(artifact: &Artifact, held: &Bitfield) -> u64 {
    (0..artifact.chunk_count())
        .filter(|&index| held.get(index))
        .filter_map(|index| artifact.locate(index))
        .map(|location| u64::from(location.len))
        .sum()
}

/// Fetches what was found into `out_dir`, resuming from whatever is already
/// there. Everything that happens is reported to `observer`, as a copy
/// from any other kind of place reports it.
///
/// # Errors
///
/// Returns [`Error::Stalled`] if no chunk arrives within the stall timeout,
/// and [`Error::Place`] for local disk failures.
pub async fn fetch(
    node: &Arc<Node>,
    found: Found,
    out_dir: &Path,
    options: DownloadOptions,
    progress: Arc<Progress>,
    observer: Observer,
) -> Result<Report> {
    let started = Instant::now();
    let artifact = &found.artifact;
    progress
        .total
        .store(artifact.total_size(), Ordering::Relaxed);
    let files = artifact.entries().iter().filter(|e| e.is_file()).count();
    observer(Event::Planned {
        bytes: artifact.total_size(),
        files: files as u64,
    });

    progress.set_phase(Phase::Scanning);
    let scan_progress = progress.clone();
    let folder = Arc::new(Folder::inside(artifact.tree().clone(), out_dir));
    let scan_observer = observer.clone();
    let examined = move |bytes| {
        scan_progress.scanned.fetch_add(bytes, Ordering::Relaxed);
        scan_observer(Event::Examined { bytes });
    };
    let secret = found.access.secret;
    let hints = fetch_hints(node, &found.manifest_peer, artifact, &secret);
    let store = Store::resume_with_hints(artifact.clone(), folder.clone(), examined, hints).await?;
    let have = store.held();

    let resumed = bytes_of(artifact, &have);
    progress.resumed.store(resumed, Ordering::Relaxed);
    observer(Event::Kept { bytes: resumed });

    let share = Arc::new(Share::new(
        artifact.clone(),
        store,
        folder.clone(),
        found.access.clone(),
    ));
    // Only in a swarm does this node answer for what it fetches. Otherwise
    // the share is this download's alone, and no request reaches it.
    let share = if found.swarm() {
        node.register(share)
    } else {
        share
    };

    progress.set_phase(Phase::Downloading);
    let (swarm, suppliers) = found.suppliers(node, true);
    let not_peers: Vec<SupplierId> = found.others.iter().map(|other| other.id()).collect();
    let limits = engine::Options {
        max_suppliers: options.max_peers,
        stall_timeout: options.stall_timeout,
        // A peer is asked for as many chunks at once as its route has room
        // for, and not for a set number.
        ..engine::Options::default()
    };
    let watching = progress.clone();
    let observer: Observer = Arc::new(move |event| {
        watching.observe(event);
        observer(event);
    });
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
    let (others, sources): (Vec<_>, Vec<_>) = summary
        .sources
        .into_iter()
        .partition(|(id, _)| not_peers.contains(id));
    Ok(Report {
        skipped_links: folder.skipped_links(),
        root: folder.root().to_owned(),
        share,
        resumed_bytes: resumed,
        fetched_bytes: progress.fetched_bytes(),
        sources: sources
            .into_iter()
            .map(|(id, bytes)| (NodeId::from_bytes(id), bytes))
            .collect(),
        others,
        peers,
        elapsed: started.elapsed(),
    })
}

/// Keeps this node listed with `peers` as a holder of `share`, which is a
/// swarm's, so that downloaders arriving later are told about it. Runs
/// until cancelled.
pub async fn announce(node: Arc<Node>, share: Arc<Share>, peers: Vec<PeerInfo>) {
    let (content, secret) = (share.artifact().content_id(), share.secret());
    let mut ticker = tokio::time::interval(ANNOUNCE_INTERVAL);
    loop {
        ticker.tick().await;
        for peer in &peers {
            let attempt = async {
                let connection = node.endpoint().connect(peer).await?;
                let status = connection.status(content, &secret, true).await;
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

/// A peer that cannot provide hints can still supply verified chunks.
async fn fetch_hints(
    node: &Node,
    peer: &PeerInfo,
    artifact: &Artifact,
    secret: &Secret,
) -> Option<model::search::ChunkHints> {
    let connection = timeout(MANIFEST_TIMEOUT, node.endpoint().connect(peer))
        .await
        .ok()?
        .ok()?;
    let hints = connection
        .hints(artifact, secret, MANIFEST_TIMEOUT)
        .await
        .ok()
        .flatten();
    connection.close();
    hints
}

async fn fetch_manifest(
    node: &Node,
    ticket: &Ticket,
    peers: &[PeerInfo],
) -> Result<(Artifact, PeerInfo)> {
    let id = ticket.artifact;
    for attempt in 0..MANIFEST_ATTEMPTS {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        // A peer that does not let this machine in will not let it in a
        // moment later. If no other peer answers, that is why.
        let mut refused = false;
        for peer in peers {
            let request = async {
                let connection = node.endpoint().connect(peer).await?;
                let encoded = connection.manifest(id, &ticket.secret).await;
                connection.close();
                encoded
            };
            let encoded = match timeout(MANIFEST_TIMEOUT, request).await {
                Ok(Ok(Some(encoded))) => encoded,
                Ok(Ok(None)) => {
                    debug!(peer = %peer.node.short(), "peer does not serve this artifact");
                    continue;
                }
                Ok(Err(Error::Refused)) => {
                    debug!(peer = %peer.node.short(), "peer does not let this machine fetch");
                    refused = true;
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
                Ok(artifact) => return Ok((artifact, peer.clone())),
                Err(e) => warn!(peer = %peer.node.short(), "rejected manifest: {e}"),
            }
        }
        if refused {
            return Err(Error::Refused);
        }
    }
    Err(Error::ManifestUnavailable)
}
