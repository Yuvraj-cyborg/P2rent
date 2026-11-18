//! The wire protocol.
//!
//! Every request travels on its own bidirectional QUIC stream. The requester
//! sends one framed [`Request`] and closes its side; the responder answers
//! with one framed [`Response`], followed by raw payload bytes when the
//! response announces them. A frame is a big-endian `u32` length and a
//! postcard-encoded message.

use crate::error::{Error, Result};
use model::artifact::{Bitfield, Hash};
use model::place::Arrived;
use model::ticket::NodeId;
use quinn::{RecvStream, SendStream};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

/// Protocol identifier negotiated during the TLS handshake.
pub const ALPN: &[u8] = b"mux/3";

/// Requests are tiny; anything larger is a misbehaving peer.
pub const MAX_REQUEST_BYTES: usize = 1024;

/// Bounds a response frame. The largest is a status reply, whose bitfield
/// needs one bit per chunk.
pub const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

/// A request, sent as the first frame on a new stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Request {
    /// The encoded manifest of an artifact.
    Manifest {
        /// The id of the artifact whose manifest is wanted.
        artifact: Hash,
    },
    /// Which chunks the peer holds and which other peers it knows. Sending
    /// this also registers the requester as a participant.
    Status {
        /// The content id being asked about. Peers that hold the same
        /// content under different names, permissions or times are one
        /// swarm.
        content: Hash,
    },
    /// One chunk.
    Chunk {
        /// The content id the chunk belongs to.
        content: Hash,
        /// Position of the chunk in the content.
        index: u64,
    },
}

/// The first frame of an answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Response {
    /// Followed by `len` bytes of encoded manifest.
    Manifest {
        /// Length of the encoded manifest that follows.
        len: u64,
    },
    /// What the peer holds and whom it knows.
    Status(Status),
    /// Followed by `len` bytes of chunk data.
    Chunk {
        /// Length of the chunk that follows.
        len: u32,
    },
    /// The peer does not have what was asked for.
    NotFound,
}

/// What a peer holds of an artifact and whom else it knows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    /// Chunks the peer holds.
    pub have: Have,
    /// Other participants the peer has seen recently.
    pub peers: Vec<PeerInfo>,
}

/// A set of chunks, with a short form for a complete artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Have {
    /// Every chunk.
    All,
    /// The chunks whose bits are set.
    Some(Bitfield),
}

impl Have {
    /// Expands into a bitfield of `chunks` bits, rejecting malformed input.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Protocol`] if the bitfield is malformed or does not describe `chunks`
    /// chunks.
    pub fn into_bitfield(self, chunks: u64) -> Result<Bitfield> {
        match self {
            Have::All => Ok(Bitfield::full(chunks)),
            Have::Some(bits) if bits.is_well_formed(chunks) => Ok(bits),
            Have::Some(_) => Err(Error::Protocol("malformed chunk bitfield".into())),
        }
    }
}

/// How to reach another participant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PeerInfo {
    /// The participant's node id.
    pub node: NodeId,
    /// Where it can be dialled. Only the IP address and port are sent: an
    /// IPv6 scope id or flow label is local to one machine.
    pub addr: SocketAddr,
}

/// Writes one length-prefixed message.
///
/// # Errors
///
/// Returns [`Error::Protocol`] if the message cannot be encoded, and a transport error if the
/// stream fails.
pub async fn write_frame<T: Serialize>(send: &mut SendStream, message: &T) -> Result<()> {
    let body = postcard::to_allocvec(message).map_err(|e| Error::Protocol(e.to_string()))?;
    let len = u32::try_from(body.len()).map_err(|_| Error::Protocol("frame too large".into()))?;
    send.write_all(&len.to_be_bytes()).await?;
    send.write_all(&body).await?;
    Ok(())
}

/// Reads one length-prefixed message of at most `max` bytes.
///
/// # Errors
///
/// Returns [`Error::Protocol`] if the frame is too large or cannot be decoded, and a transport
/// error if the stream fails.
pub async fn read_frame<T: DeserializeOwned>(recv: &mut RecvStream, max: usize) -> Result<T> {
    let mut len = [0u8; 4];
    recv.read_exact(&mut len).await?;
    let len = u32::from_be_bytes(len) as usize;
    if len > max {
        return Err(Error::Protocol(format!(
            "frame of {len} bytes exceeds the {max} byte limit"
        )));
    }
    let body = read_payload(recv, len, &Arrived::default()).await?;
    postcard::from_bytes(&body).map_err(|e| Error::Protocol(e.to_string()))
}

/// Reads exactly `len` raw bytes, and adds them to `arrived` as they come.
///
/// # Errors
///
/// Returns a transport error if the stream ends early.
pub async fn read_payload(recv: &mut RecvStream, len: usize, arrived: &Arrived) -> Result<Vec<u8>> {
    let mut payload = vec![0u8; len];
    let mut filled = 0;
    while filled < len {
        let read = recv.read(&mut payload[filled..]).await?;
        let read = read.ok_or_else(|| Error::Transport("the stream ended early".into()))?;
        filled += read;
        arrived.add(read);
    }
    Ok(payload)
}
