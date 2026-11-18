//! Answers requests from other peers.

use super::node::Node;
use crate::error::{Error, Result};
use crate::net::Connection;
use crate::net::proto::{self, Have, Request, Response, Status};
use iroh::endpoint::{RecvStream, SendStream};
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
    let request: Request = timeout(
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
    let small = !matches!(request, Request::Chunk { .. });
    send.set_priority(if small { 1 } else { order })?;

    match request {
        Request::Manifest { artifact } => match node.share_of_artifact(artifact) {
            Some(share) => {
                let encoded = share.artifact().encoded();
                let len = encoded.len() as u64;
                proto::write_frame(&mut send, &Response::Manifest { len }).await?;
                send.write_all(encoded).await?;
            }
            None => proto::write_frame(&mut send, &Response::NotFound).await?,
        },

        Request::Status { content } => match node.share(content) {
            Some(share) => {
                share.note_participant(connection.peer_info());
                let have = share.have();
                let status = Status {
                    have: if have.is_full() {
                        Have::All
                    } else {
                        Have::Some(have)
                    },
                    peers: share.participants(connection.peer()),
                };
                proto::write_frame(&mut send, &Response::Status(status)).await?;
            }
            None => proto::write_frame(&mut send, &Response::NotFound).await?,
        },

        Request::Chunk { content, index } => {
            // Never closed, so a turn always comes.
            let _turn = turns.acquire().await;
            let share = node.share(content).filter(|share| share.has(index));
            let data = match share {
                Some(share) => match share.store().read_chunk(index).await {
                    Ok(data) => Some((share, data)),
                    Err(e) => {
                        warn!(%content, index, "cannot read chunk: {e}");
                        None
                    }
                },
                None => None,
            };
            match data {
                Some((share, data)) => {
                    let len = u32::try_from(data.len())
                        .map_err(|_| Error::Protocol("chunk too large".into()))?;
                    proto::write_frame(&mut send, &Response::Chunk { len }).await?;
                    send.write_all(&data).await?;
                    share.add_uploaded(u64::from(len));
                }
                None => proto::write_frame(&mut send, &Response::NotFound).await?,
            }
        }
    }

    send.finish()?;
    Ok(())
}
