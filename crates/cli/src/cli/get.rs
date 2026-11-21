//! `mux get`: `mux cp TICKET FOLDER`, under the name it had first.

use super::cp::{self, Get};
use super::{Reach, Sharing};
use mux::swarm::{Ticket, TicketPeer};
use std::path::{MAIN_SEPARATOR, Path, PathBuf};

#[derive(clap::Args)]
pub(super) struct Args {
    /// Ticket printed by `mux seed`, or by `mux cp` to `peer:`
    ticket: Ticket,

    /// Directory to download into; existing data there is reused
    #[arg(short, long, default_value = ".", value_name = "DIR")]
    out: PathBuf,

    /// Go on offering the artifact to other machines after the download:
    /// in its swarm if the ticket is a swarm's, and under a ticket of its
    /// own if not
    #[arg(long)]
    seed: bool,

    #[command(flatten)]
    sharing: Sharing,

    /// Additional peer to try, by its node id and, if known, where it is
    /// (repeatable)
    #[arg(long = "peer", value_name = "NODE[@HOST:PORT]")]
    peers: Vec<TicketPeer>,

    #[command(flatten)]
    reach: Reach,

    /// Peers to download from at once
    #[arg(long, default_value_t = 8, value_name = "N")]
    max_peers: usize,

    /// Give up after this long without receiving any data
    #[arg(long, default_value_t = 60, value_name = "SECONDS")]
    stall_timeout: u64,
}

pub(super) async fn run(args: Args, json: bool, key: Option<&Path>) -> anyhow::Result<()> {
    // A peer named beside the ticket is one more peer of the ticket.
    let mut ticket = args.ticket;
    ticket.peers.extend(args.peers);
    // Written as a folder, so that one that is not there yet is made.
    let folder = format!("{}{MAIN_SEPARATOR}", args.out.display());
    let told = Get {
        seed: args.seed,
        max_peers: args.max_peers,
        stall_timeout: args.stall_timeout,
        reach: args.reach,
        sharing: args.sharing,
    };
    cp::run(cp::Args::get(ticket.to_string(), folder, told), json, key).await
}
