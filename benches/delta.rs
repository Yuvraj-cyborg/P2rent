//! Which way of sending only the changed parts of a file to choose.
//!
//! A destination holds an older version of a file, and the source a newer
//! one. Three ways of finding what the destination can keep are tried on
//! three kinds of change, and for each the bytes that would still have to
//! be fetched are counted:
//!
//! - **same place**: the new file is cut into chunks of one size, and a
//!   chunk is kept if the old file has the same bytes at the same place.
//!   This is what a transfer does today when the digests are known.
//! - **search**: the same chunks, and a chunk is kept if the old file has
//!   its bytes anywhere. The old file is gone through once with a hash
//!   that rolls from each byte to the next, as `rsync` and `zsync` do.
//! - **cut by content**: both files are cut where their bytes say, so that
//!   a cut falls in the same place after something is inserted before it.
//!   A chunk is kept if the old file has a chunk with the same digest.
//!
//! | Variable            | Default | Meaning          |
//! | ------------------- | ------- | ---------------- |
//! | `MUX_BENCH_SIZE_MB` | 256     | Size of the file |
//!
//! Everything is held in memory: allow about three times the size.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

const MIB: usize = 1024 * 1024;
/// The size of a chunk today, and the middle size of one cut by content.
const CHUNK: usize = MIB;
/// The same, as the rolling hash counts it.
const WINDOW: u32 = 1 << 20;
/// How many values of the rolling hash the search keeps a bit for.
const FILTER_BITS: usize = 1 << 20;
/// How much is changed: appended, overwritten, or inserted.
const CHANGE: usize = 4096;

fn main() {
    let megabytes = match std::env::var("MUX_BENCH_SIZE_MB") {
        Ok(value) => value
            .parse::<usize>()
            .unwrap_or_else(|_| panic!("MUX_BENCH_SIZE_MB must be a whole number, got {value:?}")),
        Err(_) => 256,
    };
    let size = megabytes * MIB;
    let old = bytes(size, b"the old file");
    let fresh = bytes(size / 100, b"what is new");
    println!(
        "a file of {} MiB, in chunks of {} KiB",
        size / CHUNK,
        CHUNK / 1024
    );

    let grown = [&old[..], &fresh[..]].concat();
    let mut edited = old.clone();
    edited[size / 2..size / 2 + CHANGE].copy_from_slice(&fresh[..CHANGE]);
    let third = size / 3;
    let inserted = [&old[..third], &fresh[..CHANGE], &old[third..]].concat();

    println!();
    println!("bytes to fetch              same place        search   cut by content   cut, 64 KiB");
    for (change, new) in [
        ("grown by 1%", &grown),
        ("4 KiB changed in the middle", &edited),
        ("4 KiB inserted at a third", &inserted),
    ] {
        println!(
            "{change:<27} {:>10} {:>13} {:>16} {:>13}",
            kib(same_place(&old, new)),
            kib(search(&old, new).to_fetch),
            kib(cut_by_content(&old, new, CHUNK)),
            kib(cut_by_content(&old, new, 64 * 1024)),
        );
    }

    println!();
    println!("how fast the old file is gone through");
    let started = Instant::now();
    let searched = search(&old, &inserted);
    println!(
        "search          {}   ({} of {} places where the hash matched were not the chunk)",
        rate(size, started.elapsed()),
        searched.false_matches,
        searched.matches,
    );
    let started = Instant::now();
    let cuts = cuts(&old, CHUNK).len();
    println!(
        "cut by content  {}   ({cuts} chunks)",
        rate(size, started.elapsed())
    );
    let started = Instant::now();
    let digest = blake3::hash(&old);
    println!(
        "BLAKE3, to compare with: {}   ({})",
        rate(size, started.elapsed()),
        &digest.to_hex()[..8]
    );

    println!();
    println!("what a source has to say of each GiB, beside the bytes");
    let per_gib = 1024 * MIB;
    println!(
        "same place      {} KiB: a digest for each chunk",
        per_gib / CHUNK * 32 / 1024
    );
    println!(
        "search          {} KiB: and a rolling hash for each chunk",
        per_gib / CHUNK * 40 / 1024
    );
    println!(
        "cut by content  {} KiB: a digest and a length for each chunk",
        per_gib / CHUNK * 36 / 1024
    );
    println!("cut, 64 KiB     {} KiB", per_gib / (64 * 1024) * 36 / 1024);
}

/// `len` bytes that do not repeat and cannot be compressed.
fn bytes(len: usize, seed: &[u8]) -> Vec<u8> {
    let mut made = vec![0; len];
    blake3::Hasher::new()
        .update(seed)
        .finalize_xof()
        .fill(&mut made);
    made
}

fn kib(bytes: usize) -> String {
    format!("{} KiB", bytes.div_ceil(1024))
}

fn rate(bytes: usize, elapsed: Duration) -> String {
    let megabytes = bytes as f64 / MIB as f64;
    format!("{:>6.0} MiB/s", megabytes / elapsed.as_secs_f64())
}

/// The bytes of the chunks of `new` that `old` does not have at the same
/// place.
fn same_place(old: &[u8], new: &[u8]) -> usize {
    let chunks = new.chunks(CHUNK).enumerate();
    chunks
        .filter(|(index, chunk)| {
            old.get(index * CHUNK..index * CHUNK + chunk.len()) != Some(*chunk)
        })
        .map(|(_, chunk)| chunk.len())
        .sum()
}

/// What a search of the old file found.
struct Searched {
    /// The bytes of the chunks of the new file that the old one has
    /// nowhere.
    to_fetch: usize,
    /// How many times the rolling hash matched that of a chunk.
    matches: usize,
    /// How many of those were not the chunk after all.
    false_matches: usize,
}

/// A hash of one chunk's worth of bytes that can be moved on by one byte
/// without looking at the bytes in between: each byte is multiplied by a
/// power of one odd number, the higher the longer the byte has to stay.
///
/// It has 64 bits. The checksum of `rsync` has 32, which is enough for the
/// small blocks it uses and not for this: a file of 100 GiB has a hundred
/// thousand chunks to look for at a hundred thousand million places, and
/// 32 bits would match by chance millions of times, each to be ruled out
/// by hashing a chunk.
#[derive(Clone, Copy)]
struct Rolling(u64);

/// The odd number.
const FACTOR: u64 = 0x9e37_79b9_7f4a_7c15;

impl Rolling {
    /// The hash of one chunk.
    fn of(window: &[u8]) -> Self {
        let mut hash = 0u64;
        for &byte in window {
            hash = hash.wrapping_mul(FACTOR).wrapping_add(u64::from(byte));
        }
        Self(hash)
    }

    /// `FACTOR` to the power of one less than the length of a chunk: what
    /// the byte that leaves was multiplied by.
    fn leaving() -> u64 {
        let mut power = 1u64;
        for _ in 1..WINDOW {
            power = power.wrapping_mul(FACTOR);
        }
        power
    }

    /// Moves the window on by one byte: `out` leaves and `inn` comes in.
    fn roll(&mut self, leaving: u64, out: u8, inn: u8) {
        let without = self.0.wrapping_sub(leaving.wrapping_mul(u64::from(out)));
        self.0 = without.wrapping_mul(FACTOR).wrapping_add(u64::from(inn));
    }

    fn value(self) -> u64 {
        self.0
    }

    /// Which bit of the filter stands for this value. The top bits are
    /// used: they have been mixed by the most multiplications.
    fn bit(self) -> usize {
        // Twenty bits, so the number fits whatever the size of a pointer.
        (self.0 >> 44) as usize
    }
}

/// Looks through `old` for every whole chunk of `new`, wherever it lies.
/// The last chunk of a file is short, and is looked for only at its own
/// place.
fn search(old: &[u8], new: &[u8]) -> Searched {
    // The whole chunks of the new file, by their rolling hash.
    let mut wanted: HashMap<u64, Vec<usize>> = HashMap::new();
    // Nearly every place in the old file is the start of no chunk. A bit
    // for each of a million values of the hash says so at once, and
    // the table is asked only where a bit is set.
    let mut maybe = vec![0u64; FILTER_BITS / 64];
    let (whole, tail) = new.as_chunks::<CHUNK>();
    for (index, chunk) in whole.iter().enumerate() {
        let hash = Rolling::of(chunk);
        wanted.entry(hash.value()).or_default().push(index);
        maybe[hash.bit() / 64] |= 1 << (hash.bit() % 64);
    }
    let digest = |chunk: &[u8]| *blake3::hash(chunk).as_bytes();
    let digests: Vec<[u8; 32]> = whole.iter().map(|chunk| digest(chunk)).collect();

    let mut found = vec![false; digests.len()];
    let (mut matches, mut false_matches) = (0, 0);
    if old.len() >= CHUNK {
        let leaving = Rolling::leaving();
        let mut sum = Rolling::of(&old[..CHUNK]);
        let mut at = 0;
        loop {
            let possible = maybe[sum.bit() / 64] & (1 << (sum.bit() % 64)) != 0;
            if let Some(candidates) = possible.then(|| wanted.get(&sum.value())).flatten() {
                matches += 1;
                let here = digest(&old[at..at + CHUNK]);
                let mut any = false;
                for &index in candidates {
                    if digests[index] == here {
                        found[index] = true;
                        any = true;
                    }
                }
                false_matches += usize::from(!any);
            }
            if at + CHUNK >= old.len() {
                break;
            }
            sum.roll(leaving, old[at], old[at + CHUNK]);
            at += 1;
        }
    }

    let missing = found.iter().filter(|found| !**found).count() * CHUNK;
    let tail_at = new.len() - tail.len();
    let tail_kept = old.get(tail_at..tail_at + tail.len()) == Some(tail);
    Searched {
        to_fetch: missing + if tail_kept { 0 } else { tail.len() },
        matches,
        false_matches,
    }
}

/// Where a file is cut when the cuts are chosen by its bytes: after each
/// byte at which a hash of the bytes before it has its low bits clear.
/// Chunks are between a quarter of `middle` and four times it.
fn cuts(file: &[u8], middle: usize) -> Vec<usize> {
    // A number for each value of a byte, the same every time.
    let mut table = [0u64; 256];
    let mut seed = [0u8; 256 * 8];
    blake3::Hasher::new()
        .update(b"gear")
        .finalize_xof()
        .fill(&mut seed);
    for (entry, bytes) in table.iter_mut().zip(seed.as_chunks::<8>().0) {
        *entry = u64::from_le_bytes(*bytes);
    }
    // The hash is cut where its top bits are clear: the top ones have
    // been mixed by the most bytes.
    let mask = !(u64::MAX >> middle.trailing_zeros());
    let (least, most) = (middle / 4, middle * 4);

    let mut ends = Vec::new();
    let (mut start, mut hash) = (0, 0u64);
    for (at, &byte) in file.iter().enumerate() {
        hash = (hash << 1).wrapping_add(table[usize::from(byte)]);
        let len = at + 1 - start;
        if len >= most || (len >= least && hash & mask == 0) {
            ends.push(at + 1);
            start = at + 1;
            hash = 0;
        }
    }
    if start < file.len() {
        ends.push(file.len());
    }
    ends
}

/// The bytes of the chunks of `new`, cut by content, that `old` cut the
/// same way does not have.
fn cut_by_content(old: &[u8], new: &[u8], middle: usize) -> usize {
    let chunks = |file: &[u8]| -> Vec<(usize, [u8; 32])> {
        let mut start = 0;
        let cut = |end: usize| {
            let chunk = &file[start..end];
            start = end;
            (chunk.len(), *blake3::hash(chunk).as_bytes())
        };
        cuts(file, middle).into_iter().map(cut).collect()
    };
    let held: HashSet<[u8; 32]> = chunks(old).into_iter().map(|(_, digest)| digest).collect();
    let lacking = chunks(new)
        .into_iter()
        .filter(|(_, digest)| !held.contains(digest));
    lacking.map(|(len, _)| len).sum()
}
