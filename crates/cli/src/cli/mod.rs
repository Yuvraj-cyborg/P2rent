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
use mux::net::Identity;
use std::path::{Path, PathBuf};
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

#[derive(Subcommand)]
enum Command {
    /// Copy from one place to another
    Cp(cp::Args),
    /// Say what is at a place, without copying it
    Probe(probe::Args),
    /// Make a folder hold what a place holds
    Sync(sync::Args),
    /// Offer files or directories to other machines
    Seed(seed::Args),
    /// Download an artifact from a swarm
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
        Command::Cp(args) => cp::run(args, cli.json).await,
        Command::Probe(args) => probe::run(&args, cli.json).await,
        Command::Sync(args) => sync::run(&args, cli.json).await,
        Command::Seed(args) => seed::run(args, &identity(cli.key.as_deref())?).await,
        Command::Get(args) => get::run(args, &identity(cli.key.as_deref())?).await,
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
