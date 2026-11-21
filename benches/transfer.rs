//! End-to-end transfer benchmark over loopback.
//!
//! Seeds a generated artifact from one or more nodes and downloads it with a
//! fresh node, several times. Every node runs on its own runtime so that the
//! seeders and the downloader do not share worker threads, as they would not
//! in separate processes.
//!
//! Configure with environment variables:
//!
//! | Variable                | Default | Meaning                             |
//! | ----------------------- | ------- | ----------------------------------- |
//! | `MUX_BENCH_SIZE_MB`  | 1024    | Artifact size                       |
//! | `MUX_BENCH_FILES`    | 1       | Split the artifact into this many   |
//! | `MUX_BENCH_SEEDERS`  | 1       | Nodes serving the artifact          |
//! | `MUX_BENCH_RUNS`     | 3       | Downloads to time                   |
//! | `MUX_BENCH_DIR`      | temp    | Where to put the data               |
//!
//! Loopback has no packet loss, no latency and no network card, so these
//! numbers measure CPU cost per byte, not what a real network will deliver.

mod timing;

use ::fs as store;
use model::artifact::DEFAULT_CHUNK_SIZE;
use peer::net::{Identity, Options, Relays};
use peer::swarm::{Access, DownloadOptions, Node, Progress, Ticket, download};
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use timing::{MIB, Runs, cpu_time, rate, setting};
use tokio::runtime::Runtime;

fn main() {
    let size = setting("MUX_BENCH_SIZE_MB", 1024) * MIB;
    let files = setting("MUX_BENCH_FILES", 1).max(1);
    let seeders = setting("MUX_BENCH_SEEDERS", 1).max(1);
    let runs = setting("MUX_BENCH_RUNS", 3).max(1);

    let dir = match std::env::var_os("MUX_BENCH_DIR") {
        Some(dir) => tempfile::tempdir_in(dir),
        None => tempfile::tempdir(),
    }
    .expect("cannot create a working directory");

    let source = generate(dir.path(), size, files);
    let started = Instant::now();
    let indexed = store::index(&source, DEFAULT_CHUNK_SIZE, &|_| {}).expect("cannot index");
    let index_time = started.elapsed();
    let artifact = Arc::new(indexed.artifact);
    println!(
        "artifact  {} in {} file(s), {} chunks",
        mib(size),
        files,
        artifact.chunk_count()
    );
    println!("index     {}", rate(size, index_time));

    // Seeders stay up for the whole benchmark, each on its own runtime.
    // Reached at one local address, and through no relay.
    let localhost = Options {
        listen: Some("127.0.0.1:0".parse().unwrap()),
        relays: Relays::Off,
        ..Options::default()
    };
    // One offer, held by every seeder: the ticket names them all.
    let access = Access::new();
    let mut ticket = Ticket {
        artifact: artifact.id(),
        secret: access.secret,
        swarm: false,
        peers: Vec::new(),
        origins: Vec::new(),
    };
    let mut seeding = Vec::new();
    for _ in 0..seeders {
        let runtime = Runtime::new().expect("cannot start a runtime");
        let node = runtime
            .block_on(Node::bind(&Identity::generate(), &localhost))
            .expect("cannot start a seeder");
        let share = node.seed(artifact.clone(), indexed.root.clone(), access.clone());
        let addr = node.local_addrs()[0].to_string();
        ticket.peers.extend(node.ticket(&share, &[addr]).peers);
        seeding.push((runtime, node));
    }

    println!("download  from {seeders} seeder(s), {runs} run(s)");
    // CPU time covers every node in this process, seeders included.
    let mut timed = Runs::new(size);
    for run in 1..=runs {
        let out = dir.path().join(format!("out-{run}"));
        let runtime = Runtime::new().expect("cannot start a runtime");
        let cpu_before = cpu_time();
        let report = runtime
            .block_on(async {
                let node = Node::bind(&Identity::generate(), &localhost).await?;
                let report = download(
                    &node,
                    &ticket,
                    &out,
                    DownloadOptions::default(),
                    Arc::new(Progress::default()),
                )
                .await;
                node.shutdown().await;
                report
            })
            .expect("download failed");
        assert_eq!(report.fetched_bytes, size, "download was not complete");
        timed.record(run, report.elapsed, cpu_before);
        drop(runtime);
        std::fs::remove_dir_all(&out).expect("cannot remove the downloaded copy");
    }
    timed.report();

    for (runtime, node) in seeding {
        runtime.block_on(node.shutdown());
        runtime.shutdown_timeout(Duration::from_secs(1));
    }
}

/// Writes `size` bytes of incompressible data, as one file or as a directory
/// of `files` equal parts, and returns the path to seed.
fn generate(dir: &Path, size: u64, files: u64) -> PathBuf {
    let mut stream = blake3::Hasher::new().update(b"mux bench").finalize_xof();
    let mut block = vec![0u8; 4 << 20];
    let mut write = |path: &Path, mut remaining: u64| {
        let mut file = File::create(path).expect("cannot create bench data");
        while remaining > 0 {
            let n = usize::try_from(remaining).map_or(block.len(), |r| r.min(block.len()));
            stream.fill(&mut block[..n]);
            file.write_all(&block[..n])
                .expect("cannot write bench data");
            remaining -= n as u64;
        }
    };

    if files == 1 {
        let path = dir.join("artifact.bin");
        write(&path, size);
        return path;
    }
    let root = dir.join("artifact");
    std::fs::create_dir(&root).expect("cannot create bench data");
    for i in 0..files {
        let share = size / files + u64::from(i < size % files);
        write(&root.join(format!("part-{i:05}.bin")), share);
    }
    root
}

fn mib(bytes: u64) -> String {
    format!("{} MiB", bytes / MIB)
}
