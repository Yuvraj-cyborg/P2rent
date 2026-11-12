//! The command-line interface. Everything here is presentation; the engine
//! lives in the library.

mod get;
mod seed;
mod ui;

use anyhow::Context;
use clap::{ArgAction, Parser, Subcommand};
use p2rent::net::Identity;
use std::path::PathBuf;
use std::process::ExitCode;
use tracing::Level;

#[derive(Parser)]
#[command(
    name = "p2rent",
    version,
    about = "Swarm distribution of large artifacts over QUIC",
    propagate_version = true
)]
struct Cli {
    /// Log more (-v for info, -vv for debug)
    #[arg(short, long, action = ArgAction::Count, global = true)]
    verbose: u8,

    /// Node key file [default: <config dir>/p2rent/node.key]
    #[arg(long, global = true, env = "P2RENT_KEY", value_name = "FILE")]
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

pub fn main() -> ExitCode {
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
    let key_path = cli.key.unwrap_or_else(Identity::default_path);
    let identity = Identity::load_or_create(&key_path)
        .with_context(|| format!("cannot load the node key at {}", key_path.display()))?;

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
