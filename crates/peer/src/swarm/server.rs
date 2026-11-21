//! Answers requests from other peers.

use super::node::Node;
use super::share::{Admission, Share};
use crate::error::{Error, Result};
use crate::net::Connection;
use crate::net::proto::{self, Call, Have, Request, Response, Status};
use iroh::endpoint::{RecvStream, SendStream};
use model::access::Proof;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
use tokio::time::timeout;
use tracing::{debug, warn};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Chunks being read from disk for one connection at a time. A peer may ask
/// for many more at once, and the rest wait their turn.
const CHUNKS_AT_ONCE: usize = 4;

/// Accepts connections until the endpoint closes.
pub(crate) async fn run(node: Arc<Node>) {
    while let Some(incoming) = node.endpoint().accept().await {
        let node = node.clone();
        // Each handshake runs in its own task: a client that stalls only
        // delays itself.
        tokio::spawn(async move {
            match timeout(HANDSHAKE_TIMEOUT, incoming.establish()).await {
                Ok(Ok(connection)) => serve_connection(node, connection).await,
                Ok(Err(e)) => debug!("inbound handshake failed: {e}"),
                Err(_) => debug!("inbound handshake timed out"),
            }
        });
    }
}

async fn serve_connection(node: Arc<Node>, connection: Connection) {
    debug!(peer = %connection.peer().short(), at = ?connection.peer_info(), "peer connected");
    let turns = Arc::new(Semaphore::new(CHUNKS_AT_ONCE));
    // Lower for each request, so that what was asked for first is sent
    // first.
    let mut order: i32 = 0;
    while let Ok((send, recv)) = connection.accept_request().await {
        order = order.saturating_sub(1);
        let node = node.clone();
        let connection = connection.clone();
        let turns = turns.clone();
        tokio::spawn(async move {
            let served = serve_request(&node, &connection, &turns, order, send, recv).await;
            if let Err(e) = served {
                debug!(peer = %connection.peer().short(), "request failed: {e}");
            }
        });
    }
}

async fn serve_request(
    node: &Node,
    connection: &Connection,
    turns: &Semaphore,
    order: i32,
    mut send: SendStream,
    mut recv: RecvStream,
) -> Result<()> {
    let call: Call = timeout(
        REQUEST_TIMEOUT,
        proto::read_frame(&mut recv, proto::MAX_REQUEST_BYTES),
    )
    .await
    .map_err(|_| Error::Transport("timed out waiting for a request".into()))??;

    // A chunk goes out ahead of the chunks asked for after it. The
    // connection then carries one chunk at a time, which costs both ends
    // less than a slice of each in turn. A small answer goes out ahead of
    // every chunk: on a slow link a chunk is on its way for a long time,
    // and whoever asked what this peer holds would give up waiting.
    let request = call.request;
    let small = matches!(request, Request::Status { .. } | Request::Manifest { .. });
    send.set_priority(if small { 1 } else { order })?;

    match asked_for(node, connection, &request, &call.proof) {
        Ok(share) => answer(connection, turns, &share, request, &mut send).await?,
        Err(turned_away) => proto::write_frame(&mut send, &turned_away).await?,
    }
    send.finish()?;
    Ok(())
}

/// The share a request is about, if the requester may have it. Every
/// request passes here before any of it is answered.
///
/// A requester that has not shown that it knows the share's secret is told
/// what it would be told if this node held nothing of the kind.
fn asked_for(
    node: &Node,
    connection: &Connection,
    request: &Request,
    proof: &Proof,
) -> std::result::Result<Arc<Share>, Response> {
    let share = match request {
        Request::Manifest { artifact } => node.share_of_artifact(*artifact),
        Request::Status { content, .. }
        | Request::Hints { content }
        | Request::Chunk { content, .. } => node.share(*content),
    };
    let share = share.ok_or(Response::NotFound)?;
    match share.admit(&node.node_id(), &connection.peer(), proof) {
        Admission::Admitted => Ok(share),
        Admission::Refused => Err(Response::Refused),
        Admission::Unproven => Err(Response::NotFound),
    }
}

/// Answers a request about `share` from a requester that may have it.
async fn answer(
    connection: &Connection,
    turns: &Semaphore,
    share: &Share,
    request: Request,
    send: &mut SendStream,
) -> Result<()> {
    match request {
        Request::Manifest { .. } => {
            let encoded = share.artifact().encoded();
            let len = encoded.len() as u64;
            proto::write_frame(send, &Response::Manifest { len }).await?;
            send.write_all(encoded).await?;
        }

        Request::Status { listed, .. } => {
            if listed {
                share.note_participant(connection.peer_info());
            }
            let have = share.have();
            let status = Status {
                have: if have.is_full() {
                    Have::All
                } else {
                    Have::Some(have)
                },
                peers: share.participants(connection.peer()),
            };
            proto::write_frame(send, &Response::Status(status)).await?;
        }

        Request::Hints { .. } if share.is_complete() => {
            proto::write_frame(send, &Response::Hints).await?;
            for page in 0..share.hint_pages() {
                let hints = share.hint_page(page).await?;
                send.write_all(hints.encoded()).await?;
            }
        }
        Request::Hints { .. } => proto::write_frame(send, &Response::NotFound).await?,

        Request::Chunk { content, index } => {
            // Never closed, so a turn always comes.
            let _turn = turns.acquire().await;
            let data = if share.has(index) {
                let read = share.store().read_chunk(index).await;
                read.inspect_err(|e| warn!(%content, index, "cannot read chunk: {e}"))
                    .ok()
            } else {
                None
            };
            match data {
                Some(data) => {
                    let len = u32::try_from(data.len())
                        .map_err(|_| Error::Protocol("chunk too large".into()))?;
                    proto::write_frame(send, &Response::Chunk { len }).await?;
                    send.write_all(&data).await?;
                    share.add_uploaded(u64::from(len));
                }
                None => proto::write_frame(send, &Response::NotFound).await?,
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{Identity, Options, PeerInfo, Relays};
    use crate::swarm::Access;
    use model::access::Secret;
    use model::artifact::MIN_CHUNK_SIZE;

    async fn node() -> Arc<Node> {
        let options = Options {
            listen: Some("127.0.0.1:0".parse().unwrap()),
            relays: Relays::Off,
            ..Options::default()
        };
        Node::bind(&Identity::generate(), &options).await.unwrap()
    }

    /// A node that offers a file of three chunks, and the share.
    async fn offering(tmp: &std::path::Path) -> (Arc<Node>, Arc<Share>) {
        let path = tmp.join("model.bin");
        std::fs::write(&path, vec![7u8; MIN_CHUNK_SIZE as usize * 3]).unwrap();
        let indexed = fs::index(&path, MIN_CHUNK_SIZE, &|_| {}).unwrap();
        let seeder = node().await;
        let share = seeder.seed(Arc::new(indexed.artifact), indexed.root, Access::new());
        (seeder, share)
    }

    /// Only a download asks for hints, and it has the secret. So that they
    /// are for nobody else is seen here, where they can be asked for alone.
    #[tokio::test]
    async fn chunk_hints_are_for_who_knows_the_secret() {
        let tmp = tempfile::tempdir().unwrap();
        let (seeder, share) = offering(tmp.path()).await;
        let asker = node().await;
        let at = PeerInfo::at(seeder.node_id(), seeder.local_addrs()[0]);
        let connection = asker.endpoint().connect(&at).await.unwrap();

        let wait = Duration::from_secs(10);
        let wrong = Secret::from_bytes([0; 16]);
        let hints = |secret| connection.hints(share.artifact(), secret, wait);
        assert!(hints(&wrong).await.unwrap().is_none());
        let secret = share.secret();
        assert!(hints(&secret).await.unwrap().is_some());
    }

    /// A proof is made for one machine to show to one other. Shown by any
    /// other machine, or to any other, it is no proof.
    #[tokio::test]
    async fn a_proof_is_of_use_to_the_machine_it_was_made_for_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let (seeder, share) = offering(tmp.path()).await;
        let me = seeder.node_id();
        let id = || Identity::generate().node_id();
        let (guest, thief) = (id(), id());

        let proof = share.secret().proof(&me, &guest);
        assert_eq!(share.admit(&me, &guest, &proof), Admission::Admitted);
        // A machine that was shown the proof, or saw it pass, shows it.
        assert_eq!(share.admit(&me, &thief, &proof), Admission::Unproven);
        // The machine it was made for shows it to another that holds the
        // same share.
        assert_eq!(share.admit(&thief, &guest, &proof), Admission::Unproven);
        // And the machine it was shown to shows it back.
        assert_eq!(share.admit(&guest, &me, &proof), Admission::Unproven);
    }
}
