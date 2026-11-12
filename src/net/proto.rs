//! The wire protocol.
//!
//! Every request travels on its own bidirectional QUIC stream. The requester
//! sends one framed [`Request`] and closes its side; the responder answers
//! with one framed [`Response`], followed by raw payload bytes when the
//! response announces them. A frame is a big-endian `u32` length and a
//! postcard-encoded message.

use super::identity::NodeId;
use crate::artifact::{Bitfield, Hash};
use crate::error::{Error, Result};
use quinn::{RecvStream, SendStream};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

pub const ALPN: &[u8] = b"p2rent/2";

/// Requests are tiny; anything larger is a misbehaving peer.
pub const MAX_REQUEST_BYTES: usize = 1024;

/// Bounds a response frame. The largest is a status reply, whose bitfield
/// needs one bit per chunk.
pub const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Request {
    /// The encoded manifest of an artifact.
    Manifest { artifact: Hash },
    /// Which chunks the peer holds and which other peers it knows. Sending
    /// this also registers the requester as a participant.
    Status { artifact: Hash },
    /// One chunk.
    Chunk { artifact: Hash, index: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Response {
    /// Followed by `len` bytes of encoded manifest.
    Manifest {
        len: u64,
    },
    Status(Status),
    /// Followed by `len` bytes of chunk data.
    Chunk {
        len: u32,
    },
    /// The peer does not have what was asked for.
    NotFound,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub have: Have,
    pub peers: Vec<PeerInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Have {
    All,
    Some(Bitfield),
}

impl Have {
    /// Expands into a bitfield of `chunks` bits, rejecting malformed input.
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
    pub node: NodeId,
    pub addr: SocketAddr,
}

pub async fn write_frame<T: Serialize>(send: &mut SendStream, message: &T) -> Result<()> {
    let body = postcard::to_allocvec(message).map_err(|e| Error::Protocol(e.to_string()))?;
    let len = u32::try_from(body.len()).map_err(|_| Error::Protocol("frame too large".into()))?;
    send.write_all(&len.to_be_bytes()).await?;
    send.write_all(&body).await?;
    Ok(())
}

pub async fn read_frame<T: DeserializeOwned>(recv: &mut RecvStream, max: usize) -> Result<T> {
    let mut len = [0u8; 4];
    recv.read_exact(&mut len).await?;
    let len = u32::from_be_bytes(len) as usize;
    if len > max {
        return Err(Error::Protocol(format!(
            "frame of {len} bytes exceeds the {max} byte limit"
        )));
    }
    let body = read_payload(recv, len).await?;
    postcard::from_bytes(&body).map_err(|e| Error::Protocol(e.to_string()))
}

pub async fn read_payload(recv: &mut RecvStream, len: usize) -> Result<Vec<u8>> {
    let mut payload = vec![0u8; len];
    recv.read_exact(&mut payload).await?;
    Ok(payload)
}
