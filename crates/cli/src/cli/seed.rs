use super::tell::{self, Line};
use super::{Reach, ui};
use anyhow::{Context, bail};
use mux::artifact::{DEFAULT_CHUNK_SIZE, MAX_CHUNK_SIZE, MIN_CHUNK_SIZE};
use mux::net::{Identity, Relays};
use mux::store;
use mux::swarm::{Node, Share};
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// How long to wait for a relay before printing a ticket without one.
const RELAY_WAIT: Duration = Duration::from_secs(5);

#[derive(clap::Args)]
pub(super) struct Args {
    /// Files or directories to offer
    #[arg(required = true, value_name = "PATH")]
    paths: Vec<PathBuf>,

    #[command(flatten)]
    reach: Reach,

    /// Address to put in the ticket for other machines to dial (repeatable)
    /// [default: this machine's own, if no relay is used]
    #[arg(long, value_name = "HOST[:PORT]")]
    advertise: Vec<String>,

    /// Chunk size, between 4K and 16M
    #[arg(long, default_value = "1M", value_parser = ui::parse_size, value_name = "SIZE")]
    chunk_size: u32,
}

impl Args {
    /// What `mux cp PATH... peer:` means: these paths, offered as `seed`
    /// offers them when it is told nothing more.
    pub(super) fn of(paths: Vec<PathBuf>, reach: Reach) -> Self {
        Self {
            paths,
            reach,
            advertise: Vec::new(),
            chunk_size: DEFAULT_CHUNK_SIZE,
        }
    }
}

pub(super) async fn run(args: Args, identity: &Identity, json: bool) -> anyhow::Result<()> {
    if !(MIN_CHUNK_SIZE..=MAX_CHUNK_SIZE).contains(&args.chunk_size) {
        bail!(
            "chunk size must be between {} and {} (default {})",
            ui::bytes(u64::from(MIN_CHUNK_SIZE)),
            ui::bytes(u64::from(MAX_CHUNK_SIZE)),
            ui::bytes(u64::from(DEFAULT_CHUNK_SIZE)),
        );
    }

    // Bind first so a busy port is reported before any hashing starts.
    let relays = args.reach.relays();
    let node = Node::bind(identity, &args.reach.options())
        .await
        .context("cannot open a socket to listen on")?;
    let bound = node.local_addrs();
    // With no relay, a ticket has to say where this machine is.
    let detect = relays == Relays::Off;
    let advertised = advertised_addresses(&args.advertise, &bound, detect);
    if !detect
        && tokio::time::timeout(RELAY_WAIT, node.online())
            .await
            .is_err()
    {
        eprintln!(
            "note: no relay answered within {} seconds; a machine behind a router may not reach this one",
            RELAY_WAIT.as_secs()
        );
    }

    let mut shares: Vec<Arc<Share>> = Vec::new();
    for path in &args.paths {
        let share = index_and_seed(&node, path, args.chunk_size).await?;
        let artifact = share.artifact();
        eprintln!(
            "seeding  {}  ({}, {})",
            artifact.name(),
            ui::bytes(artifact.total_size()),
            ui::plural(artifact.chunk_count(), "chunk"),
        );
        // The ticket is the only thing on stdout, so it can be piped.
        let ticket = node.ticket(artifact.id(), &advertised).to_string();
        if json {
            let files = artifact.entries().iter().filter(|e| e.is_file()).count();
            tell::say(&Line::Offer {
                ticket: &ticket,
                name: artifact.name(),
                bytes: artifact.total_size(),
                files: files as u64,
            });
        } else {
            println!("{ticket}");
        }
        shares.push(share);
    }

    eprintln!("node     {}", node.node_id());
    let listed = |addrs: &[String]| match addrs {
        [] => "nothing".to_owned(),
        addrs => addrs.join(", "),
    };
    let bound: Vec<String> = bound.iter().map(ToString::to_string).collect();
    eprintln!(
        "listen   {}  (advertised as {})",
        listed(&bound),
        listed(&advertised)
    );
    serve_until_interrupted(&shares).await;
    node.shutdown().await;
    Ok(())
}

async fn index_and_seed(node: &Node, path: &Path, chunk_size: u32) -> anyhow::Result<Arc<Share>> {
    let total = store::measure(path).with_context(|| format!("cannot read {}", path.display()))?;
    let bar = ui::bytes_bar(total, "hashing");
    let hashed = Arc::new(AtomicU64::new(0));

    let task = tokio::task::spawn_blocking({
        let path = path.to_path_buf();
        let hashed = hashed.clone();
        move || {
            store::index(&path, chunk_size, &|bytes| {
                hashed.fetch_add(bytes, Ordering::Relaxed);
            })
        }
    });
    tokio::pin!(task);
    let indexed = loop {
        tokio::select! {
            joined = &mut task => break joined,
            () = tokio::time::sleep(Duration::from_millis(100)) => {
                bar.set_position(hashed.load(Ordering::Relaxed));
            }
        }
    };
    bar.finish_and_clear();
    let indexed = indexed
        .context("hashing was interrupted")?
        .with_context(|| format!("cannot index {}", path.display()))?;

    if indexed.skipped > 0 {
        let entries = match indexed.skipped {
            1 => "1 entry".to_owned(),
            n => format!("{n} entries"),
        };
        eprintln!(
            "note: left out {entries}. Links that point outside the tree, unfinished \
             downloads and special files are not sent."
        );
    }
    Ok(node.seed(Arc::new(indexed.artifact), indexed.root))
}

async fn serve_until_interrupted(shares: &[Arc<Share>]) {
    let status = ui::spinner("waiting for peers (Ctrl-C to stop)");
    let interrupted = tokio::signal::ctrl_c();
    tokio::pin!(interrupted);
    loop {
        tokio::select! {
            _ = &mut interrupted => break,
            () = tokio::time::sleep(Duration::from_millis(500)) => {
                let uploaded: u64 = shares.iter().map(|s| s.uploaded_bytes()).sum();
                if uploaded > 0 {
                    status.set_message(format!("uploaded {} (Ctrl-C to stop)", ui::bytes(uploaded)));
                }
            }
        }
    }
    status.finish_and_clear();
    let uploaded: u64 = shares.iter().map(|s| s.uploaded_bytes()).sum();
    eprintln!("stopped after uploading {}", ui::bytes(uploaded));
}

/// The `host:port` strings to put in tickets: those asked for, at the port
/// listened on if they name none. With none asked for, and if told to
/// `detect`, the address this machine has on its network.
fn advertised_addresses(requested: &[String], bound: &[SocketAddr], detect: bool) -> Vec<String> {
    // A machine is told an IPv4 address unless it listens on IPv6 alone.
    let Some(bound) = bound.iter().find(|addr| addr.is_ipv4()).or(bound.first()) else {
        return requested.to_vec();
    };
    if !requested.is_empty() {
        let with_port = |entry: &String| with_default_port(entry, bound.port());
        return requested.iter().map(with_port).collect();
    }
    if !detect {
        return Vec::new();
    }
    let ip = if bound.ip().is_unspecified() {
        primary_ip().unwrap_or_else(|| {
            eprintln!(
                "note: could not detect this machine's address; pass --advertise so other machines can reach it"
            );
            IpAddr::from([127, 0, 0, 1])
        })
    } else {
        bound.ip()
    };
    vec![SocketAddr::new(ip, bound.port()).to_string()]
}

fn with_default_port(entry: &str, port: u16) -> String {
    if entry.parse::<SocketAddr>().is_ok() {
        return entry.to_owned();
    }
    if let Ok(ip) = entry.parse::<IpAddr>() {
        return SocketAddr::new(ip, port).to_string();
    }
    let has_port = entry
        .rsplit_once(':')
        .is_some_and(|(_, port)| port.parse::<u16>().is_ok());
    if has_port {
        entry.to_owned()
    } else {
        format!("{entry}:{port}")
    }
}

/// The local address used for outbound traffic. Connecting a UDP socket picks
/// a route without sending anything.
fn primary_ip() -> Option<IpAddr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_unspecified()).then_some(ip)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advertised_entries_get_the_listen_port_when_missing() {
        assert_eq!(with_default_port("10.0.0.5", 4433), "10.0.0.5:4433");
        assert_eq!(with_default_port("10.0.0.5:9000", 4433), "10.0.0.5:9000");
        assert_eq!(with_default_port("fd00::1", 4433), "[fd00::1]:4433");
        assert_eq!(with_default_port("[fd00::1]:9000", 4433), "[fd00::1]:9000");
        assert_eq!(
            with_default_port("seed.internal", 4433),
            "seed.internal:4433"
        );
        assert_eq!(
            with_default_port("seed.internal:9000", 4433),
            "seed.internal:9000"
        );
    }

    #[test]
    fn a_specific_listen_address_is_advertised_as_is() {
        let bound: [SocketAddr; 1] = ["192.168.1.20:4433".parse().unwrap()];
        assert_eq!(
            advertised_addresses(&[], &bound, true),
            ["192.168.1.20:4433"]
        );
    }

    #[test]
    fn with_a_relay_only_what_was_asked_for_is_advertised() {
        let bound: [SocketAddr; 2] = [
            "[::]:7000".parse().unwrap(),
            "0.0.0.0:4433".parse().unwrap(),
        ];
        assert_eq!(advertised_addresses(&[], &bound, false), [""; 0]);
        // What is asked for gets the port of the IPv4 socket.
        let asked = ["seed.example".to_owned(), "10.0.0.5:9000".to_owned()];
        assert_eq!(
            advertised_addresses(&asked, &bound, false),
            ["seed.example:4433", "10.0.0.5:9000"]
        );
        // With no socket there is no port to add.
        assert_eq!(advertised_addresses(&asked, &[], true), asked);
    }
}
