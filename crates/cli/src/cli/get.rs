use super::ui;
use anyhow::Context;
use mux::net::Identity;
use mux::swarm::{DownloadOptions, Node, Phase, Progress, Report, Ticket, TicketPeer};
use mux::swarm::{announce, download};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

#[derive(clap::Args)]
pub(super) struct Args {
    /// Ticket printed by `mux seed`
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

    /// How to decide how fast to send to other machines
    #[arg(long, value_enum, default_value = "cubic", value_name = "HOW")]
    congestion: super::Congestion,
}

pub(super) async fn run(args: Args, identity: &Identity) -> anyhow::Result<()> {
    let mut ticket = args.ticket;
    ticket.peers.extend(args.peers);

    let node = Node::bind_with(identity, args.listen, args.congestion.into())
        .with_context(|| format!("cannot listen on {}", args.listen))?;
    let options = DownloadOptions {
        max_peers: args.max_peers,
        stall_timeout: Duration::from_secs(args.stall_timeout),
    };

    let report = download_with_progress(&node, &ticket, &args.out, options).await?;
    print_summary(&report);

    if args.seed {
        let content = report.share.artifact().content_id();
        let announcer = tokio::spawn(announce(node.clone(), content, report.peers.clone()));
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
            () = tokio::time::sleep(Duration::from_millis(100)) => {}
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
    outcome.with_context(|| match progress.phase() {
        // Whatever arrived is on disk and verified, so a retry continues.
        Phase::Downloading => "download interrupted; run the same command again to resume",
        _ => "download failed",
    })
}

fn print_summary(report: &Report) {
    let artifact = report.share.artifact();
    eprintln!(
        "saved    {}  ({})",
        report.root.display(),
        ui::bytes(artifact.total_size()),
    );
    if report.resumed_bytes > 0 {
        eprintln!(
            "reused   {} already on disk",
            ui::bytes(report.resumed_bytes)
        );
    }
    if report.skipped_links > 0 {
        eprintln!(
            "skipped  {}, which this system cannot create",
            ui::plural(report.skipped_links, "link")
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
