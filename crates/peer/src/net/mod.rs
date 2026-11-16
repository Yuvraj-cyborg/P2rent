//! Networking: node identity, authenticated QUIC connections and the wire
//! protocol. This layer knows nothing about disks or swarms.

mod endpoint;
mod identity;
pub mod proto;
mod tls;

pub use endpoint::{Connection, Endpoint, Incoming};
pub use identity::{Identity, NodeId};
pub use proto::PeerInfo;
