//! The swarm engine: what a node offers, how it answers peers, and how it
//! downloads an artifact from many of them at once.

mod download;
mod node;
mod server;
mod share;

pub use download::{DownloadOptions, Phase, Progress, Report, announce, download};
pub use model::ticket::{Ticket, TicketPeer};
pub use node::Node;
pub use share::Share;
