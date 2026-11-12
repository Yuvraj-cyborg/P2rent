use super::ui;
use anyhow::Context;
use p2rent::net::Identity;
use p2rent::swarm::{DownloadOptions, Node, Phase, Progress, Report, Ticket, TicketPeer};
use p2rent::swarm::{announce, download};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

#[derive(clap::Args)]
pub struct Args {
    /// Ticket printed by `p2rent seed`
    ticket: Ticket,

    /// Directory to download into; existing data there is reused
    #[arg(short, long, default_value = ".", value_name = "DIR")]
    out: PathBuf,

    /// Keep serving the artifact to other machines after the download
    #[arg(long)]
    seed: bool,

    /// Additional peer to try (repeatable)
    #[arg(long = "peer", value_name = "NODE@HOST:PORT")]
    peers: Vec<TicketPeer>,

    /// Address to listen on; other downloaders fetch from this node here
    #[arg(long, default_value = "0.0.0.0:0", value_name = "ADDR")]
    listen: SocketAddr,

    /// Peers to download from at once
    #[arg(long, default_value_t = 8, value_name = "N")]
    max_peers: usize,

    /// Give up after this long without receiving any data
    #[arg(long, default_value_t = 60, value_name = "SECONDS")]
    stall_timeout: u64,
}

pub async fn run(args: Args, identity: &Identity) -> anyhow::Result<()> {
    let mut ticket = args.ticket;
    ticket.peers.extend(args.peers);

    let node = Node::bind(identity, args.listen)
        .with_context(|| format!("cannot listen on {}", args.listen))?;
    let options = DownloadOptions {
        max_peers: args.max_peers,
        stall_timeout: Duration::from_secs(args.stall_timeout),
        ..DownloadOptions::default()
    };

    let report = download_with_progress(&node, &ticket, &args.out, options).await?;
    print_summary(&report);

    if args.seed {
        let artifact = report.share.artifact().id();
        let announcer = tokio::spawn(announce(node.clone(), artifact, report.peers.clone()));
        let status = ui::spinner(format!(
            "seeding on {} (Ctrl-C to stop)",
            node.local_addr()?
        ));
        let _ = tokio::signal::ctrl_c().await;
        status.finish_and_clear();
        announcer.abort();
        eprintln!(
            "stopped after uploading {}",
            ui::bytes(report.share.uploaded_bytes())
        );
    }
    node.shutdown();
    Ok(())
}

async fn download_with_progress(
    node: &Arc<Node>,
    ticket: &Ticket,
    out: &std::path::Path,
    options: DownloadOptions,
) -> anyhow::Result<Report> {
    let progress = Arc::new(Progress::default());
    let transfer = download(node, ticket, out, options, progress.clone());
    tokio::pin!(transfer);

    let mut phase = Phase::Connecting;
    let mut bar = ui::spinner("finding peers");
    let outcome = loop {
        tokio::select! {
            outcome = &mut transfer => break outcome,
            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
        }
        let current = progress.phase();
        if current != phase {
            phase = current;
            bar.finish_and_clear();
            bar = match phase {
                Phase::Scanning => ui::bytes_bar(progress.total_bytes(), "checking"),
                Phase::Downloading => {
                    let bar = ui::bytes_bar(progress.total_bytes(), "downloading");
                    // Data found on disk should not count towards throughput.
                    bar.set_position(progress.resumed_bytes());
                    bar.reset_eta();
                    bar
                }
                Phase::Connecting | Phase::Done => ui::spinner("finishing"),
            };
        }
        match phase {
            Phase::Scanning => bar.set_position(progress.scanned_bytes()),
            Phase::Downloading => {
                bar.set_position(progress.done_bytes());
                bar.set_message(ui::plural(progress.peers(), "peer"));
            }
            Phase::Connecting | Phase::Done => {}
        }
    };
    bar.finish_and_clear();
    outcome.context("download failed; run the same command again to resume")
}

fn print_summary(report: &Report) {
    let artifact = report.share.artifact();
    eprintln!(
        "saved    {}  ({})",
        report.share.store().root().display(),
        ui::bytes(artifact.total_size()),
    );
    if report.resumed_bytes > 0 {
        eprintln!(
            "reused   {} already on disk",
            ui::bytes(report.resumed_bytes)
        );
    }
    if report.fetched_bytes > 0 {
        eprintln!(
            "fetched  {} in {:.1?} ({}) from {}",
            ui::bytes(report.fetched_bytes),
            report.elapsed,
            ui::rate(report.fetched_bytes, report.elapsed),
            ui::plural(report.sources.len(), "peer"),
        );
        for (node, bytes) in &report.sources {
            eprintln!("         {}  {}", node.short(), ui::bytes(*bytes));
        }
    }
}
