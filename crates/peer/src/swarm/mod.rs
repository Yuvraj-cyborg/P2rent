//! The swarm engine: what a node offers, how it answers peers, and how it
//! downloads an artifact from many of them at once.

mod download;
mod node;
mod server;
mod share;

pub use download::{
    DownloadOptions, Found, Holder, Phase, Progress, Report, announce, download, fetch, find,
    holders,
};
pub use model::access::{Guests, Secret};
pub use model::ticket::{Ticket, TicketPeer, Whereabouts};
pub use node::Node;
pub use share::{Access, Share};
