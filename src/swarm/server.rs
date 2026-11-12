//! Answers requests from other peers.

use super::node::Node;
use crate::error::{Error, Result};
use crate::net::proto::{self, Have, Request, Response, Status};
use crate::net::{Connection, PeerInfo};
use quinn::{RecvStream, SendStream};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::timeout;
use tracing::{debug, warn};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

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
    debug!(peer = %connection.peer().short(), addr = %connection.remote_addr(), "peer connected");
    while let Ok((send, recv)) = connection.accept_request().await {
        let node = node.clone();
        let connection = connection.clone();
        tokio::spawn(async move {
            if let Err(e) = serve_request(&node, &connection, send, recv).await {
                debug!(peer = %connection.peer().short(), "request failed: {e}");
            }
        });
    }
}

async fn serve_request(
    node: &Node,
    connection: &Connection,
    mut send: SendStream,
    mut recv: RecvStream,
) -> Result<()> {
    let request: Request = timeout(
        REQUEST_TIMEOUT,
        proto::read_frame(&mut recv, proto::MAX_REQUEST_BYTES),
    )
    .await
    .map_err(|_| Error::Transport("timed out waiting for a request".into()))??;

    match request {
        Request::Manifest { artifact } => match node.share(artifact) {
            Some(share) => {
                let encoded = share.artifact().encoded();
                let len = encoded.len() as u64;
                proto::write_frame(&mut send, &Response::Manifest { len }).await?;
                send.write_all(encoded).await?;
            }
            None => proto::write_frame(&mut send, &Response::NotFound).await?,
        },

        Request::Status { artifact } => match node.share(artifact) {
            Some(share) => {
                share.note_participant(PeerInfo {
                    node: connection.peer(),
                    addr: connection.remote_addr(),
                });
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

        Request::Chunk { artifact, index } => {
            let share = node.share(artifact).filter(|share| share.has(index));
            let data = match share {
                Some(share) => {
                    let reader = share.clone();
                    let read =
                        tokio::task::spawn_blocking(move || reader.store().read_chunk(index))
                            .await
                            .map_err(|e| Error::Transport(e.to_string()))?;
                    match read {
                        Ok(data) => Some((share, data)),
                        Err(e) => {
                            warn!(%artifact, index, "cannot read chunk from disk: {e}");
                            None
                        }
                    }
                }
                None => None,
            };
            match data {
                Some((share, data)) => {
                    let len = data.len() as u32;
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
