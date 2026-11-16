//! The command-line interface. Everything here is presentation; the engine
//! lives in the library.

mod get;
mod seed;
mod ui;

use anyhow::Context;
use clap::{ArgAction, Parser, Subcommand};
use mux::net::Identity;
use std::path::PathBuf;
use std::process::ExitCode;
use tracing::Level;

#[derive(Parser)]
#[command(
    name = "mux",
    version,
    about = "Swarm distribution of large artifacts over QUIC",
    propagate_version = true
)]
struct Cli {
    /// Log more (-v for info, -vv for debug)
    #[arg(short, long, action = ArgAction::Count, global = true)]
    verbose: u8,

    /// Node key file [default: <config dir>/mux/node.key]
    #[arg(long, global = true, env = "MUX_KEY", value_name = "FILE")]
    key: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Offer files or directories to other machines
    Seed(seed::Args),
    /// Download an artifact from a swarm
    Get(get::Args),
    /// Print this machine's node id
    Id,
}

pub(crate) fn main() -> ExitCode {
    let cli = Cli::parse();
    init_logging(cli.verbose);

    let outcome = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("cannot start the async runtime")
        .and_then(|runtime| runtime.block_on(run(cli)));

    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> anyhow::Result<()> {
    let identity = match &cli.key {
        Some(path) => Identity::load_or_create(path)
            .with_context(|| format!("cannot load the node key at {}", path.display())),
        // The usual place, adopting a key left by the tool's former name.
        None => Identity::load_or_create_default().with_context(|| {
            format!(
                "cannot load the node key at {}",
                Identity::default_path().display()
            )
        }),
    }?;

    match cli.command {
        Command::Seed(args) => seed::run(args, &identity).await,
        Command::Get(args) => get::run(args, &identity).await,
        Command::Id => {
            println!("{}", identity.node_id());
            Ok(())
        }
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
