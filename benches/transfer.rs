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
//! | `P2RENT_BENCH_SIZE_MB`  | 1024    | Artifact size                       |
//! | `P2RENT_BENCH_FILES`    | 1       | Split the artifact into this many   |
//! | `P2RENT_BENCH_SEEDERS`  | 1       | Nodes serving the artifact          |
//! | `P2RENT_BENCH_RUNS`     | 3       | Downloads to time                   |
//! | `P2RENT_BENCH_DIR`      | temp    | Where to put the data               |
//!
//! Loopback has no packet loss, no latency and no network card, so these
//! numbers measure CPU cost per byte, not what a real network will deliver.

use p2rent::artifact::DEFAULT_CHUNK_SIZE;
use p2rent::net::Identity;
use p2rent::store;
use p2rent::swarm::{DownloadOptions, Node, Progress, Ticket, download};
use std::fs::File;
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::runtime::Runtime;

const MIB: u64 = 1024 * 1024;

fn main() {
    let size = setting("P2RENT_BENCH_SIZE_MB", 1024) * MIB;
    let files = setting("P2RENT_BENCH_FILES", 1).max(1);
    let seeders = setting("P2RENT_BENCH_SEEDERS", 1).max(1);
    let runs = setting("P2RENT_BENCH_RUNS", 3).max(1);

    let dir = match std::env::var_os("P2RENT_BENCH_DIR") {
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
    let localhost: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut ticket = Ticket {
        artifact: artifact.id(),
        peers: Vec::new(),
    };
    let mut seeding = Vec::new();
    for _ in 0..seeders {
        let runtime = Runtime::new().expect("cannot start a runtime");
        let node = runtime
            .block_on(async { Node::bind(&Identity::generate()?, localhost) })
            .expect("cannot start a seeder");
        node.seed(artifact.clone(), indexed.root.clone());
        let addr = node.local_addr().unwrap().to_string();
        ticket
            .peers
            .extend(node.ticket(artifact.id(), &[addr]).peers);
        seeding.push((runtime, node));
    }

    println!("download  from {seeders} seeder(s), {runs} run(s)");
    let mut rates = Vec::new();
    let mut cpu_per_gib = Vec::new();
    let gib = size as f64 / (1024.0 * MIB as f64);
    for run in 1..=runs {
        let out = dir.path().join(format!("out-{run}"));
        let runtime = Runtime::new().expect("cannot start a runtime");
        let cpu_before = cpu_time();
        let report = runtime
            .block_on(async {
                let node = Node::bind(&Identity::generate()?, localhost)?;
                let report = download(
                    &node,
                    &ticket,
                    &out,
                    DownloadOptions::default(),
                    Arc::new(Progress::default()),
                )
                .await;
                node.shutdown();
                report
            })
            .expect("download failed");
        let cpu = cpu_time()
            .zip(cpu_before)
            .map(|(after, before)| (after.0 - before.0, after.1 - before.1));
        assert_eq!(report.fetched_bytes, size, "download was not complete");

        let per_second = size as f64 / MIB as f64 / report.elapsed.as_secs_f64();
        rates.push(per_second);
        match cpu {
            Some((user, system)) => {
                let (user, system) = (user.as_secs_f64() / gib, system.as_secs_f64() / gib);
                cpu_per_gib.push(user + system);
                println!(
                    "  run {run}   {}   {:.2} cpu-s/GiB  (user {user:.2}, system {system:.2})",
                    rate(size, report.elapsed),
                    user + system,
                );
            }
            None => println!("  run {run}   {}", rate(size, report.elapsed)),
        }
        drop(runtime);
        std::fs::remove_dir_all(&out).expect("cannot remove the downloaded copy");
    }

    // CPU time covers every node in this process, seeders included. It is
    // steadier than wall time, which also depends on the disk.
    rates.sort_by(|a, b| a.total_cmp(b));
    cpu_per_gib.sort_by(|a, b| a.total_cmp(b));
    match cpu_per_gib.get(cpu_per_gib.len() / 2) {
        Some(cost) => println!(
            "median    {:>5.0} MiB/s            {cost:.2} cpu-s/GiB",
            rates[rates.len() / 2]
        ),
        None => println!("median    {:>5.0} MiB/s", rates[rates.len() / 2]),
    }

    for (runtime, node) in seeding {
        node.shutdown();
        runtime.shutdown_timeout(Duration::from_secs(1));
    }
}

fn setting(name: &str, default: u64) -> u64 {
    match std::env::var(name) {
        Ok(value) => value
            .parse()
            .unwrap_or_else(|_| panic!("{name} must be a whole number, got {value:?}")),
        Err(_) => default,
    }
}

/// Writes `size` bytes of incompressible data, as one file or as a directory
/// of `files` equal parts, and returns the path to seed.
fn generate(dir: &Path, size: u64, files: u64) -> PathBuf {
    let mut stream = blake3::Hasher::new().update(b"p2rent bench").finalize_xof();
    let mut block = vec![0u8; 4 * MIB as usize];
    let mut write = |path: &Path, mut remaining: u64| {
        let mut file = File::create(path).expect("cannot create bench data");
        while remaining > 0 {
            let n = remaining.min(block.len() as u64) as usize;
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

fn rate(bytes: u64, elapsed: Duration) -> String {
    format!(
        "{:>5.0} MiB/s  ({:.2}s)",
        bytes as f64 / MIB as f64 / elapsed.as_secs_f64(),
        elapsed.as_secs_f64()
    )
}

/// User and system CPU time used by this process so far.
#[cfg(unix)]
fn cpu_time() -> Option<(Duration, Duration)> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    // SAFETY: `getrusage` fills the struct it is given and reports failure
    // through its return value.
    let usage = unsafe {
        if libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) != 0 {
            return None;
        }
        usage.assume_init()
    };
    let seconds = |time: libc::timeval| {
        Duration::from_secs(time.tv_sec as u64) + Duration::from_micros(time.tv_usec as u64)
    };
    Some((seconds(usage.ru_utime), seconds(usage.ru_stime)))
}

#[cfg(not(unix))]
fn cpu_time() -> Option<(Duration, Duration)> {
    None
}
