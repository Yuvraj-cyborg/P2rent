//! Instruction counts for the hot paths that run on one thread.
//!
//! These run under Valgrind, which counts the instructions a function
//! executes instead of timing it. The count does not depend on what else the
//! machine is doing, so a change of a few percent is real. CI compares every
//! pull request with its base branch and fails on a rise beyond the
//! threshold: see `docs/performance.md`.
//!
//! Setup functions and the dropping of returned values are not counted. Time
//! spent in the kernel is not counted either, so the disk benchmarks measure
//! what the store adds around each read and write (finding the chunk,
//! building the path, opening the file, any copying), not the disk.
//!
//! Indexing is not here. It hashes on several threads at once, and the
//! instruction count of a thread pool changes from run to run. The transfer
//! benchmark times it instead.

use fs::DiskStore;
use gungraun::prelude::*;
use gungraun::{Callgrind, EventKind};
use model::artifact::{Artifact, Bitfield, DEFAULT_CHUNK_SIZE, FileEntry, Hash, Kind, Manifest};
use model::schedule::Schedule;
use std::hint::black_box;
use std::sync::Arc;
use tempfile::TempDir;

/// One chunk's worth of bytes that are not all the same.
fn chunk_bytes() -> Vec<u8> {
    (0..DEFAULT_CHUNK_SIZE)
        .map(|i| i.to_le_bytes()[0])
        .collect()
}

/// A manifest of `files` files of `chunks_each` whole chunks each.
fn manifest(files: u64, chunks_each: u64) -> Manifest {
    Manifest {
        name: "bench".to_owned(),
        kind: Kind::Directory,
        chunk_size: DEFAULT_CHUNK_SIZE,
        files: (0..files)
            .map(|file| FileEntry {
                path: format!("dir/file-{file:06}"),
                size: chunks_each * u64::from(DEFAULT_CHUNK_SIZE),
                executable: false,
            })
            .collect(),
        chunks: (0..files * chunks_each)
            .map(|chunk| Hash::of(&chunk.to_le_bytes()))
            .collect(),
    }
}

fn artifact(files: u64, chunks_each: u64) -> Artifact {
    Artifact::new(manifest(files, chunks_each)).expect("the manifest is valid")
}

/// What a peer sends: the artifact id, and the encoded manifest.
fn encoded(files: u64, chunks_each: u64) -> (Hash, Vec<u8>) {
    let artifact = artifact(files, chunks_each);
    (artifact.id(), artifact.encoded().to_vec())
}

/// A store of one file, and one chunk's bytes.
struct OnDisk {
    store: DiskStore,
    chunk: Vec<u8>,
    chunks: u64,
    // Removes the file when dropped, which is after the measurement.
    _dir: TempDir,
}

/// A file of `chunks` chunks that are all the same. With `written`, every
/// chunk is on disk and the file has its final name. Without, the file is an
/// empty part file waiting for its chunks.
fn on_disk(chunks: u64, written: bool) -> OnDisk {
    let chunk = chunk_bytes();
    let manifest = Manifest {
        name: "bench.bin".to_owned(),
        kind: Kind::File,
        chunk_size: DEFAULT_CHUNK_SIZE,
        files: vec![FileEntry {
            path: "bench.bin".to_owned(),
            size: chunks * u64::from(DEFAULT_CHUNK_SIZE),
            executable: false,
        }],
        chunks: (0..chunks).map(|_| Hash::of(&chunk)).collect(),
    };
    let artifact = Arc::new(Artifact::new(manifest).expect("the manifest is valid"));
    let dir = tempfile::tempdir().expect("cannot create a working directory");
    let store = DiskStore::resume(artifact, dir.path(), &|_| {}).expect("cannot create the file");
    if written {
        for index in 0..chunks {
            assert!(store.write_chunk(index, &chunk).expect("cannot write"));
        }
    }
    OnDisk {
        store,
        chunk,
        chunks,
        _dir: dir,
    }
}

// Hashing one chunk: done for every chunk read from disk or the network.
#[library_benchmark]
#[bench::one_mib(setup = chunk_bytes)]
fn hash_chunk(data: Vec<u8>) -> (Hash, Vec<u8>) {
    (Hash::of(black_box(&data)), data)
}

// Validating, encoding and naming a manifest: done once per artifact seeded.
#[library_benchmark]
#[bench::ten_thousand_chunks(args = (1000, 10), setup = manifest)]
fn build_artifact(manifest: Manifest) -> Artifact {
    Artifact::new(black_box(manifest)).expect("the manifest is valid")
}

// Checking and decoding a manifest from a peer: done once per download, on
// bytes that are not trusted.
#[library_benchmark]
#[bench::ten_thousand_chunks(args = (1000, 10), setup = encoded)]
fn decode_artifact((id, bytes): (Hash, Vec<u8>)) -> Artifact {
    Artifact::decode(black_box(id), black_box(bytes)).expect("the bytes are valid")
}

// Finding the file, offset and length of every chunk: done for each chunk
// read or written.
#[library_benchmark]
#[bench::ten_thousand_chunks(args = (1000, 10), setup = artifact)]
fn locate_every_chunk(artifact: Artifact) -> (u64, Artifact) {
    let mut total = 0;
    for index in 0..artifact.chunk_count() {
        if let Some(chunk) = artifact.locate(black_box(index)) {
            total += u64::from(chunk.len);
        }
    }
    (black_box(total), artifact)
}

// Marking chunks as held and counting them: done as chunks arrive and each
// time a peer says what it has.
#[library_benchmark]
#[bench::ten_thousand_chunks(10_000)]
fn mark_and_count(chunks: u64) -> (u64, Bitfield) {
    let mut held = Bitfield::new(chunks);
    let mut index = 0;
    while index < chunks {
        held.set(black_box(index));
        index += 3;
    }
    (black_box(held.count_ones()), held)
}

// Claiming and completing every chunk of a download, one at a time.
#[library_benchmark]
#[bench::ten_thousand_chunks(10_000)]
fn claim_every_chunk(chunks: u64) -> (u64, Schedule) {
    let available = Bitfield::full(chunks);
    let mut schedule = Schedule::new(&Bitfield::new(chunks));
    let mut cursor = 0;
    let mut claimed = 0;
    while let Some(index) = schedule.claim(black_box(&available), &mut cursor) {
        schedule.complete(index);
        claimed += 1;
    }
    (black_box(claimed), schedule)
}

// Reading every chunk of a file: what a seeder does to serve it.
#[library_benchmark]
#[bench::sixteen_chunks(args = (16, true), setup = on_disk)]
fn read_chunks(disk: OnDisk) -> (usize, OnDisk) {
    let mut read = 0;
    for index in 0..disk.chunks {
        let data = disk
            .store
            .read_chunk(black_box(index))
            .expect("cannot read");
        read += data.len();
    }
    (black_box(read), disk)
}

// Checking every chunk of a file against the manifest, writing it to the
// part file, and at the last one flushing the file and renaming it into
// place: what a downloader does as chunks arrive.
#[library_benchmark]
#[bench::sixteen_chunks(args = (16, false), setup = on_disk)]
fn store_chunks(disk: OnDisk) -> OnDisk {
    for index in 0..disk.chunks {
        let kept = disk
            .store
            .write_chunk(black_box(index), black_box(&disk.chunk))
            .expect("cannot write");
        assert!(kept);
    }
    disk
}

library_benchmark_group!(
    name = pure;
    benchmarks =
        hash_chunk,
        build_artifact,
        decode_artifact,
        locate_every_chunk,
        mark_and_count,
        claim_every_chunk
);

library_benchmark_group!(
    name = disk;
    benchmarks = read_chunks, store_chunks
);

// A benchmark fails when it executes this many percent more instructions
// than the run it is compared with. The counts are exact, so any rise is
// real; the limit only decides how large a rise needs a decision.
//
// A pull request that has to raise a count beyond this gives that one
// benchmark a higher limit, with the reason beside it, and records the new
// figure in docs/performance.md.
const RISE_LIMIT_PERCENT: f64 = 5.0;

main!(
    config = LibraryBenchmarkConfig::default()
        .tool(Callgrind::default().soft_limits([(EventKind::Ir, RISE_LIMIT_PERCENT)]));
    library_benchmark_groups = pure, disk
);
