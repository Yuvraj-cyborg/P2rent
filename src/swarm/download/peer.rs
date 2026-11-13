//! Everything one download does with one remote peer.

use super::session::Session;
use crate::artifact::{Bitfield, Hash};
use crate::error::Error;
use crate::net::{Connection, PeerInfo};
use std::sync::atomic::Ordering;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::task::JoinSet;
use tokio::time::{MissedTickBehavior, timeout};
use tracing::debug;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const STATUS_TIMEOUT: Duration = Duration::from_secs(10);
const CHUNK_TIMEOUT: Duration = Duration::from_secs(60);
const STATUS_INTERVAL: Duration = Duration::from_secs(2);
const IDLE_POLL: Duration = Duration::from_secs(1);

/// Most peers accepted from a single status reply.
const MAX_PEERS_PER_STATUS: usize = 64;

/// Why work with a peer ended.
pub(super) enum Outcome {
    /// Nothing left to do: the download is complete.
    Finished,
    /// The peer was unreachable or broke the connection. Worth retrying.
    Failed(String),
    /// The peer sent data that failed verification. Never contact it again.
    Banned(String),
    /// A local problem, such as a full disk, that ends the whole download.
    Fatal(Error),
}

pub(super) async fn run(session: Arc<Session>, peer: PeerInfo) -> Outcome {
    let endpoint = session.node.endpoint();
    let connection = match timeout(CONNECT_TIMEOUT, endpoint.connect(peer.addr, peer.node)).await {
        Ok(Ok(connection)) => connection,
        Ok(Err(e)) => return Outcome::Failed(e.to_string()),
        Err(_) => return Outcome::Failed("connection attempt timed out".into()),
    };
    let _connected = Connected::new(&session, connection.clone());

    let view = Arc::new(RwLock::new(Bitfield::new(session.artifact.chunk_count())));
    if let Err(outcome) = refresh(&session, &connection, &view).await {
        return outcome;
    }

    let mut workers = JoinSet::new();
    let total = session.artifact.chunk_count().max(1);
    let origin = stagger(&session, &peer);
    for worker in 0..session.options.requests_per_peer.max(1) as u64 {
        let cursor =
            (origin + worker * total / session.options.requests_per_peer.max(1) as u64) % total;
        workers.spawn(work(
            session.clone(),
            connection.clone(),
            view.clone(),
            cursor,
        ));
    }

    let mut ticker = tokio::time::interval(STATUS_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    ticker.tick().await;
    loop {
        tokio::select! {
            joined = workers.join_next() => match joined {
                None => return Outcome::Finished,
                Some(Ok(Ok(()))) => {}
                // Dropping the set cancels the other workers, which releases
                // whatever chunks they had reserved.
                Some(Ok(Err(outcome))) => return outcome,
                Some(Err(e)) => return Outcome::Failed(format!("worker crashed: {e}")),
            },
            _ = ticker.tick() => {
                if let Err(outcome) = refresh(&session, &connection, &view).await {
                    return outcome;
                }
            }
        }
    }
}

/// Asks the peer what it holds and whom it knows. This also keeps this node
/// listed as a participant on the peer's side.
async fn refresh(
    session: &Session,
    connection: &Connection,
    view: &RwLock<Bitfield>,
) -> Result<(), Outcome> {
    let status = match timeout(STATUS_TIMEOUT, connection.status(session.artifact.id())).await {
        Ok(Ok(Some(status))) => status,
        Ok(Ok(None)) => return Err(Outcome::Failed("peer does not serve this artifact".into())),
        Ok(Err(e)) => return Err(Outcome::Failed(e.to_string())),
        Err(_) => return Err(Outcome::Failed("status request timed out".into())),
    };
    let have = status
        .have
        .into_bitfield(session.artifact.chunk_count())
        .map_err(|e| Outcome::Banned(e.to_string()))?;
    *view.write().unwrap() = have;
    for peer in status.peers.into_iter().take(MAX_PEERS_PER_STATUS) {
        session.discover(peer);
    }
    session.wake.notify_waiters();
    Ok(())
}

/// One request pipeline: claim a chunk, fetch it, verify it, write it.
async fn work(
    session: Arc<Session>,
    connection: Connection,
    view: Arc<RwLock<Bitfield>>,
    mut cursor: u64,
) -> Result<(), Outcome> {
    let artifact = session.artifact.clone();
    loop {
        if session.is_complete() {
            return Ok(());
        }
        let claim = session.claim(&view.read().unwrap(), &mut cursor);
        let Some(claim) = claim else {
            // The peer has nothing we need right now. It may gain chunks, or
            // another peer may release one it holds.
            tokio::select! {
                _ = session.wake.notified() => {}
                _ = tokio::time::sleep(IDLE_POLL) => {}
            }
            continue;
        };

        let index = claim.index;
        let (Some(location), Some(expected)) = (artifact.locate(index), artifact.chunk_hash(index))
        else {
            return Err(Outcome::Fatal(Error::Protocol(
                "chunk index out of range".into(),
            )));
        };

        let request = connection.chunk(artifact.id(), index, location.len);
        let data = match timeout(CHUNK_TIMEOUT, request).await {
            Ok(Ok(Some(data))) => data,
            Ok(Ok(None)) => {
                view.write().unwrap().clear(index);
                continue;
            }
            Ok(Err(e)) => return Err(Outcome::Failed(e.to_string())),
            Err(_) => return Err(Outcome::Failed(format!("chunk {index} timed out"))),
        };

        let share = session.share.clone();
        let stored = tokio::task::spawn_blocking(move || {
            if Hash::of(&data) != expected {
                return Ok(false);
            }
            share.store().write_chunk(index, &data).map(|()| true)
        })
        .await;
        match stored {
            Ok(Ok(true)) => session.complete(claim, connection.peer(), u64::from(location.len)),
            Ok(Ok(false)) => {
                return Err(Outcome::Banned(format!(
                    "chunk {index} failed verification"
                )));
            }
            Ok(Err(e)) => return Err(Outcome::Fatal(e.into())),
            Err(e) => return Err(Outcome::Fatal(Error::Transport(e.to_string()))),
        }
    }
}

/// A starting offset that differs per (downloader, peer) pair.
fn stagger(session: &Session, peer: &PeerInfo) -> u64 {
    let mine = session.node.node_id();
    let mut mixed = [0u8; 8];
    for (i, byte) in mixed.iter_mut().enumerate() {
        *byte = mine.as_bytes()[i] ^ peer.node.as_bytes()[i + 8];
    }
    u64::from_le_bytes(mixed)
}

/// Counts a peer as connected for as long as it lives.
struct Connected<'a> {
    session: &'a Session,
    connection: Connection,
}

impl<'a> Connected<'a> {
    fn new(session: &'a Session, connection: Connection) -> Self {
        session.progress.peers.fetch_add(1, Ordering::Relaxed);
        debug!(peer = %connection.peer().short(), addr = %connection.remote_addr(), "connected to peer");
        Self {
            session,
            connection,
        }
    }
}

impl Drop for Connected<'_> {
    fn drop(&mut self) {
        self.session.progress.peers.fetch_sub(1, Ordering::Relaxed);
        debug!(
            peer = %self.connection.peer().short(),
            path_mtu = self.connection.path_mtu(),
            "disconnected from peer"
        );
        self.connection.close();
    }
}
