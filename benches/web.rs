//! Web download benchmark.
//!
//! Fetches one web address into a folder, several times, each time from
//! nothing. The server is not part of the benchmark: it is whatever answers
//! at the address, so that `curl` and `aria2c` can be timed fetching the
//! same file from the same server.
//!
//! Configure with environment variables:
//!
//! | Variable            | Default | Meaning                                    |
//! | ------------------- | ------- | ------------------------------------------ |
//! | `MUX_BENCH_URL`     | none    | The address to fetch; nothing runs without |
//! | `MUX_BENCH_RUNS`    | 3       | Downloads to time                          |
//! | `MUX_BENCH_STREAMS` | 4       | Requests open at the same time             |
//! | `MUX_BENCH_DIR`     | temp    | Where to put the data                      |
//!
//! Against a server on the same machine there is no latency and no loss,
//! so these numbers measure CPU cost per byte, not what a real network will
//! deliver. docs/performance.md says how the comparison is run.

mod timing;

use ::fs::Folder;
use model::artifact::DEFAULT_CHUNK_SIZE;
use std::sync::Arc;
use std::time::Instant;
use timing::{Runs, cpu_time, setting};
use tokio::runtime::Runtime;
use web::{Options, Origin};

fn main() {
    let Ok(url) = std::env::var("MUX_BENCH_URL") else {
        println!("web       skipped: set MUX_BENCH_URL to the address of a large file");
        return;
    };
    let runs = setting("MUX_BENCH_RUNS", 3).max(1);
    let streams = setting("MUX_BENCH_STREAMS", 4).max(1);
    let limits = engine::Options {
        requests_per_supplier: usize::try_from(streams).unwrap_or(usize::MAX),
        ..engine::Options::default()
    };
    let dir = match std::env::var_os("MUX_BENCH_DIR") {
        Some(dir) => tempfile::tempdir_in(dir),
        None => tempfile::tempdir(),
    }
    .expect("cannot create a working directory");

    println!("download  {url}, {streams} stream(s), {runs} run(s)");
    let mut timed: Option<Runs> = None;
    for run in 1..=runs {
        let out = dir.path().join(format!("out-{run}"));
        let runtime = Runtime::new().expect("cannot start a runtime");
        let cpu_before = cpu_time();
        let started = Instant::now();
        let copied = runtime
            .block_on(async {
                let origin = Origin::find(&url, &Options::default()).await?;
                engine::copy(
                    vec![Arc::new(origin)],
                    DEFAULT_CHUNK_SIZE,
                    |tree| Arc::new(Folder::inside(tree.clone(), &out)),
                    limits.clone(),
                    Arc::new(|_| {}),
                )
                .await
            })
            .expect("download failed");
        let elapsed = started.elapsed();
        let size = copied.tree.total_size();
        assert_eq!(copied.fetched, size, "download was not complete");
        timed
            .get_or_insert_with(|| Runs::new(size))
            .record(run, elapsed, cpu_before);
        drop(runtime);
        std::fs::remove_dir_all(&out).expect("cannot remove the downloaded copy");
    }
    if let Some(timed) = timed {
        timed.report();
    }
}
