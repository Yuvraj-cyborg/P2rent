use super::ui;
use anyhow::{Context, bail};
use p2rent::artifact::{DEFAULT_CHUNK_SIZE, MAX_CHUNK_SIZE, MIN_CHUNK_SIZE};
use p2rent::net::Identity;
use p2rent::store;
use p2rent::swarm::{Node, Share};
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

#[derive(clap::Args)]
pub struct Args {
    /// Files or directories to offer
    #[arg(required = true, value_name = "PATH")]
    paths: Vec<PathBuf>,

    /// Address to listen on
    #[arg(long, default_value = "0.0.0.0:4433", value_name = "ADDR")]
    listen: SocketAddr,

    /// Address other machines should dial, if it differs from what this
    /// machine sees (repeatable)
    #[arg(long, value_name = "HOST[:PORT]")]
    advertise: Vec<String>,

    /// Chunk size, between 4K and 16M
    #[arg(long, default_value = "1M", value_parser = ui::parse_size, value_name = "SIZE")]
    chunk_size: u32,
}

pub async fn run(args: Args, identity: &Identity) -> anyhow::Result<()> {
    if !(MIN_CHUNK_SIZE..=MAX_CHUNK_SIZE).contains(&args.chunk_size) {
        bail!(
            "chunk size must be between {} and {} (default {})",
            ui::bytes(u64::from(MIN_CHUNK_SIZE)),
            ui::bytes(u64::from(MAX_CHUNK_SIZE)),
            ui::bytes(u64::from(DEFAULT_CHUNK_SIZE)),
        );
    }

    // Bind first so a busy port is reported before any hashing starts.
    let node = Node::bind(identity, args.listen)
        .with_context(|| format!("cannot listen on {}", args.listen))?;
    let bound = node.local_addr()?;
    let advertised = advertised_addresses(&args.advertise, bound);

    let mut shares: Vec<Arc<Share>> = Vec::new();
    for path in &args.paths {
        let share = index_and_seed(&node, path, args.chunk_size).await?;
        let artifact = share.artifact();
        eprintln!(
            "seeding  {}  ({}, {})",
            artifact.manifest().name,
            ui::bytes(artifact.total_size()),
            ui::plural(artifact.chunk_count() as usize, "chunk"),
        );
        // The ticket is the only thing on stdout, so it can be piped.
        println!("{}", node.ticket(artifact.id(), &advertised));
        shares.push(share);
    }

    eprintln!("node     {}", node.node_id());
    eprintln!(
        "listen   {bound}  (advertised as {})",
        advertised.join(", ")
    );
    serve_until_interrupted(&shares).await;
    node.shutdown();
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
            _ = tokio::time::sleep(Duration::from_millis(100)) => {
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
            1 => "1 entry that is not a regular file".to_owned(),
            n => format!("{n} entries that are not regular files"),
        };
        eprintln!("note: skipped {entries} (symlinks are not followed)");
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
            _ = tokio::time::sleep(Duration::from_millis(500)) => {
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

/// The `host:port` strings to put in tickets.
fn advertised_addresses(requested: &[String], bound: SocketAddr) -> Vec<String> {
    if requested.is_empty() {
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
        return vec![SocketAddr::new(ip, bound.port()).to_string()];
    }
    requested
        .iter()
        .map(|entry| with_default_port(entry, bound.port()))
        .collect()
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
    match has_port {
        true => entry.to_owned(),
        false => format!("{entry}:{port}"),
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
        let bound: SocketAddr = "192.168.1.20:4433".parse().unwrap();
        assert_eq!(advertised_addresses(&[], bound), ["192.168.1.20:4433"]);
    }
}
