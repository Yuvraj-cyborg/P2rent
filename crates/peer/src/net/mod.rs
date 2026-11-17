//! Networking: node identity, authenticated QUIC connections and the wire
//! protocol. This layer knows nothing about disks or swarms.

mod endpoint;
mod identity;
pub mod proto;

pub use endpoint::{Connection, Endpoint, Incoming};
pub use identity::Identity;
pub use model::ticket::NodeId;
pub use proto::PeerInfo;
