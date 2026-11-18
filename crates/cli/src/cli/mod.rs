//! The command-line interface. Everything here is presentation; the engine
//! lives in the library.

mod cp;
mod exit;
mod get;
mod probe;
mod seed;
mod sync;
mod tell;
mod ui;

use anyhow::Context;
use clap::{ArgAction, Parser, Subcommand};
use mux::net::{Identity, Options, Relays};
use mux::swarm::Node;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use tracing::Level;

#[derive(Parser)]
#[command(
    name = "mux",
    version,
    about = "Copies and syncs data between places: folders, the web and other machines",
    propagate_version = true
)]
struct Cli {
    /// Log more (-v for info, -vv for debug)
    #[arg(short, long, action = ArgAction::Count, global = true)]
    verbose: u8,

    /// Write what happens for a program to read: one JSON object on each
    /// line of standard output
    #[arg(long, global = true)]
    json: bool,

    /// Node key file [default: <config dir>/mux/node.key]
    #[arg(long, global = true, env = "MUX_KEY", value_name = "FILE")]
    key: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

/// How a connection to another machine decides how fast it may send.
#[derive(Clone, Copy, clap::ValueEnum)]
enum Congestion {
    /// Halve the rate at each lost packet
    Cubic,
    /// Hold the rate the link carries, whatever is lost: much faster where
    /// packets are lost
    Bbr,
}

impl From<Congestion> for mux::net::Congestion {
    fn from(chosen: Congestion) -> Self {
        match chosen {
            Congestion::Cubic => Self::Cubic,
            Congestion::Bbr => Self::Bbr,
        }
    }
}

/// How this machine and others reach each other.
#[derive(clap::Args)]
struct Reach {
    /// Address to listen on [default: every address, at a port the system picks]
    #[arg(long, value_name = "ADDR")]
    listen: Option<SocketAddr>,

    /// Reach machines behind routers through this relay, in place of the
    /// public ones
    #[arg(long, value_name = "URL", conflicts_with = "no_relay")]
    relay: Option<String>,

    /// Use no relay and no directory: a machine is reached at an address or
    /// not at all
    #[arg(long)]
    no_relay: bool,

    /// How to decide how fast to send to other machines
    #[arg(long, value_enum, default_value = "cubic", value_name = "HOW")]
    congestion: Congestion,
}

impl Reach {
    fn relays(&self) -> Relays {
        match &self.relay {
            Some(url) => Relays::At(url.clone()),
            None if self.no_relay => Relays::Off,
            None => Relays::Public,
        }
    }

    fn options(&self) -> Options {
        Options {
            listen: self.listen,
            relays: self.relays(),
            congestion: self.congestion.into(),
            ..Options::default()
        }
    }
}

#[derive(Subcommand)]
enum Command {
    /// Copy from one place to another
    #[command(override_usage = "mux cp [OPTIONS] <SOURCE>... <DESTINATION>")]
    Cp(cp::Args),
    /// Say what is at a place, without copying it
    Probe(probe::Args),
    /// Make a folder hold what a place holds
    Sync(sync::Args),
    /// Offer files or folders to other machines: `cp PATH... peer:` under
    /// its first name
    Seed(seed::Args),
    /// Fetch what a ticket names: `cp TICKET DIR/` under its first name
    Get(get::Args),
    /// Print this machine's node id
    Id,
    /// List the kinds of place this build can read and write
    Protocols,
}

pub(crate) fn main() -> ExitCode {
    let cli = Cli::parse();
    init_logging(cli.verbose);
    let json = cli.json;

    let outcome = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("cannot start the async runtime")
        .and_then(|runtime| runtime.block_on(run(cli)));

    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let exit = exit::Exit::of(&error);
            let message = format!("{error:#}");
            eprintln!("error: {message}");
            if let Some(hint) = exit.hint() {
                eprintln!("hint: {hint}");
            }
            if json {
                tell::say(&tell::Line::Error {
                    code: exit as u8,
                    kind: exit.kind(),
                    message: &message,
                    hint: exit.hint(),
                });
            }
            exit.into()
        }
    }
}

async fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Command::Cp(args) => cp::run(args, cli.json, cli.key.as_deref()).await,
        Command::Probe(args) => probe::run(&args, cli.json, cli.key.as_deref()).await,
        Command::Sync(args) => sync::run(&args, cli.json, cli.key.as_deref()).await,
        Command::Seed(args) => seed::run(args, cli.key.as_deref(), cli.json).await,
        Command::Get(args) => get::run(args, cli.json, cli.key.as_deref()).await,
        Command::Id => {
            println!("{}", identity(cli.key.as_deref())?.node_id());
            Ok(())
        }
        Command::Protocols => {
            print!("{}", ui::protocols(&mux::protocols()));
            Ok(())
        }
    }
}

/// A node of this machine, to reach other machines with: made only when a
/// command has to do with another machine, so that no other command gives
/// this one a key or opens a socket.
async fn node(key: Option<&Path>, reach: &Reach) -> anyhow::Result<Arc<Node>> {
    Node::bind(&identity(key)?, &reach.options())
        .await
        .context("cannot open a socket to listen on")
}

/// This machine's key, created on first use.
fn identity(key: Option<&Path>) -> anyhow::Result<Identity> {
    match key {
        Some(path) => Identity::load_or_create(path)
            .with_context(|| format!("cannot load the node key at {}", path.display())),
        // The usual place, adopting a key left by the tool's former name.
        None => Identity::load_or_create_default().with_context(|| {
            format!(
                "cannot load the node key at {}",
                Identity::default_path().display()
            )
        }),
    }
}

fn init_logging(verbosity: u8) {
    let level = match verbosity {
        0 => Level::WARN,
        1 => Level::INFO,
        _ => Level::DEBUG,
    };
    tracing_subscriber::fmt()
        .with_max_level(level)
        .with_writer(std::io::stderr)
        .with_target(false)
        .without_time()
        .init();
}
