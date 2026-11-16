# Performance

How speed is measured, what guards it, and what has been learnt: what
helped and what did not, so the same ground is not covered twice.

## Two kinds of measurement

| | Instruction counts | Timed transfer |
| --- | --- | --- |
| Measures | Instructions executed by one hot path | A whole transfer over loopback: MiB per second, and CPU seconds per GiB |
| Repeatable | Yes: the same build gives the same count every time | No: it moves with the machine, the disk and the temperature |
| Used as | A gate on every pull request | A record, taken every night |
| Run with | `just bench` | `just transfer` |
| Lives in | `benches/instructions.rs` | `benches/transfer.rs` |

Both are in the `benches` package, which nothing else depends on.

## Instruction counts

Each benchmark runs one hot path under Valgrind, which counts the
instructions it executes. Nothing is timed, so a busy machine does not
change the result. Setting up the input and dropping the result are not
counted, and neither is time in the kernel.

| Benchmark | Hot path | Instructions |
| --- | --- | ---: |
| `hash_chunk` | Hash one 1 MiB chunk | 2,619,342 |
| `build_artifact` | Check, encode and name a manifest of 1000 files and 10,000 chunks | 5,191,641 |
| `decode_artifact` | Check and decode the same manifest as bytes from a peer | 6,040,211 |
| `locate_every_chunk` | Find the file, offset and length of each of 10,000 chunks | 1,493,228 |
| `mark_and_count` | Set every third bit of a 10,000-bit bitfield, then count them | 67,093 |
| `claim_every_chunk` | Claim and complete each of 10,000 chunks | 580,971 |
| `read_chunks` | Read 16 chunks of 1 MiB from a file | 909,772 |
| `store_chunks` | Check 16 chunks of 1 MiB against the manifest, write them to a part file, then flush it and rename it into place | 41,930,897 |

These figures are from one build: Linux on x86-64, Rust 1.100, 17 November
2026. Another compiler or processor gives other figures, so only compare two
counts taken on the same machine with the same compiler. That is what the
gate does.

Indexing is not in the table. It hashes on several threads, and the
instruction count of a thread pool changes from run to run. The timed
transfer reports it instead.

### Running them

They need Linux, because Valgrind does not run on current macOS or on
Windows.

1. Install Valgrind 3.20 or later, from the system's packages.
2. Install the benchmark runner. Its version must match the `gungraun`
   version in `Cargo.lock`:

   ```console
   $ cargo install --locked --version 0.20.0 gungraun-runner
   ```

3. Run them:

   ```console
   $ just bench                # compare with this tree's previous run
   $ just bench origin/main    # measure main first, then compare with it
   ```

The second form checks out the other commit in a worktree of its own under
`target/`, measures it, removes the worktree and then measures this tree.

### The gate

The `bench` job in CI runs `cargo xtask bench` against the base branch of
every pull request. A benchmark that executes more than 5% more instructions
than it does on the base branch fails the job. The limit is
`RISE_LIMIT_PERCENT` in `benches/instructions.rs`.

Two builds of the same code can differ by a few instructions, because the
path of the source tree ends up in the binary. Beyond that the counts are
exact, so the limit is not there to absorb noise. It decides how large a
rise needs a decision.

A pull request that has to raise a count beyond the limit:

1. gives that one benchmark a higher limit in `benches/instructions.rs`,
   with the reason in a comment beside it;
2. updates the table above;
3. says in its description what the extra instructions buy.

A pull request that adds a hot path adds a benchmark for it. A new benchmark
has nothing to be compared with, so it passes the first time.

## Timed transfer

`just transfer` seeds a generated file from one node and downloads it with
another, both on this machine, three times. It reports throughput and CPU
seconds per GiB, and how fast the file was indexed.

```console
$ just transfer            # 1 GiB from one seeder
$ just transfer 4096 2     # 4 GiB from two seeders
```

`benches/transfer.rs` lists the settings. Put `MUX_BENCH_DIR` on a RAM disk
or tmpfs, or the disk is what gets measured.

The `Bench` workflow runs it every night on a GitHub-hosted Linux machine
and keeps the output of each run as an artifact for 90 days. That list of
runs is the history. A hosted machine is shared and its hardware changes, so
read the history for a trend over weeks and not for the difference between
two nights.

### Figures

All figures below are loopback transfers of a 1 GiB file from one seeder
unless stated. Loopback has no loss, no latency and no network card, so they
show CPU cost per byte, not what a real network delivers. Treat differences
under about 15% as noise.

| Machine | Throughput | CPU seconds per GiB | Indexing |
| --- | ---: | ---: | ---: |
| Linux, Intel Core i5-10200H (4 cores), data on tmpfs, 17 November 2026 | 441 MiB/s | 6.82 | 11,114 MiB/s |

The sections that follow come from one laptop (Apple M4) and a Linux
container running on it.

## Where the CPU goes

**macOS.** About 70% of CPU time is system time, and it tracks the number of
packets, not the number of bytes. macOS sends and receives one UDP packet per
system call.

| Datagram size | Throughput | CPU seconds per GiB |
| ---: | ---: | ---: |
| 1452 bytes | 394 MiB/s | 8.50 |
| 4000 bytes | 880 MiB/s | 4.15 |
| 9000 bytes | 1391 MiB/s | 2.66 |

**Linux.** quinn hands the kernel many packets per call, so the same
transfer runs at 680 to 830 MiB/s and 5 to 6 CPU seconds per GiB without any
tuning. A `perf` profile shows no single hotspot. Roughly a fifth of samples
are BLAKE3, a fifth are the kernel copying data to and from user space,
under a tenth is AES-GCM, and the rest is spread across QUIC bookkeeping and
scheduling. That profile includes the benchmark's own data generation and
indexing, so read it as proportions, not exact shares.

## Changes that helped

| Change | Platform | Effect |
| --- | --- | --- |
| Datagrams up to 8952 bytes when path MTU discovery allows | macOS | 365 → about 1350 MiB/s on loopback; needs a route with a large MTU |
| The same | Linux | Within noise |
| Batched UDP calls (`fast-apple-datapath`) | macOS | 16–21% at the standard datagram size, 20–30% with large datagrams |

## Changes that did not help

Each was built and measured, then dropped.

| Change | Result |
| --- | --- |
| Keep files open instead of reopening per chunk | No change in throughput or CPU (8.6 vs 9.1 CPU seconds per GiB, inside the noise) |
| Reuse chunk buffers instead of allocating per request | No change (3.74 vs 3.71 CPU seconds per GiB) |
| Larger UDP socket buffers (2 to 7 MiB) | Fewer lost packets with three seeders, but no gain in throughput |
| More seeders on loopback | Throughput stays flat; the downloader is the limit |

## Part files

Downloads used to be written straight into files under their final names.
They now go to a part file that is flushed and renamed into place when whole
([#14](https://github.com/Yuvraj-cyborg/mux/issues/14)). Measured on Linux
(Intel Core i5-10200H), before and after:

| Measurement | Before | After |
| --- | ---: | ---: |
| One 1 GiB file, tmpfs: throughput | 436 MiB/s | 435 MiB/s |
| One 1 GiB file, tmpfs: CPU seconds per GiB | 6.85 | 6.78 |
| 2000 files of 128 KiB, btrfs: CPU seconds per GiB | 8.07 | 9.94 |
| `build_artifact`, instructions | 5,149,624 | 5,191,641 (+0.8%) |
| `decode_artifact`, instructions | 5,998,194 | 6,040,211 (+0.7%) |
| `read_chunks`, instructions | 945,405 | 909,772 (-3.8%) |

For a large file nothing changes. The manifest checks rose by under one
percent, which is the new rule that no name may end in `.mux-part`.

Many small files cost about a quarter more CPU, all of it in the kernel.
Each file is now flushed and renamed as its last chunk arrives, while other
files are still being written, where before every file was flushed in one
pass at the end. Wall time for that case is not in the table: on this disk it
ranged from 7 to 190 MiB/s between identical runs, before and after.

One version was built, measured and dropped:

| Change | Result |
| --- | --- |
| Flush the directory after every rename | CPU for 2000 small files rose from 8.1 to 13.1 seconds per GiB, and throughput fell several times over. Directories are now flushed once, when the download completes. A power cut before that can undo a rename, and the next run finds the file whole under its part name and renames it again. |

`store_chunks` replaces `write_chunks`. The old benchmark measured a bare
write of 7,701 instructions for 16 chunks. Storing a chunk now includes
checking it against the manifest, which a download always did, one step
earlier, so the figure is nearly all hashing: 16 times `hash_chunk`.

## The fast path

`unsafe`, SIMD intrinsics and assembly are allowed in one crate, `accel`,
under the rules in [CONTRIBUTING.md](../CONTRIBUTING.md#7-safe-by-default-with-a-fenced-fast-path).
The crate does not exist yet: the first pull request that needs it creates
it. Every attempt is recorded here with its figures, whether it is kept or
not.

| Function | Reference | Fast version | Gain | Kept |
| --- | --- | --- | --- | --- |
| None yet | | | | |

## Method notes

- Compare CPU seconds per GiB first. Wall time on a laptop moves with disk
  state and temperature; on a nearly full SSD it ranged from 85 to 360 MiB/s
  between identical runs. Put `MUX_BENCH_DIR` on a RAM disk or tmpfs.
- Alternate the builds being compared (A, B, A, B) so drift does not look
  like a result.
- macOS `sample` reports where threads spend wall time, including time
  blocked in a system call. It does not show CPU use. An early reading of it
  pointed at file opens and reads; measurement showed they cost nothing.
- For a real CPU profile, run the benchmark under `perf` on Linux.

## Not yet measured

- Two machines over a real network, at 1, 10 or 25 Gbit/s.
- x86 servers, where BLAKE3 and AES-GCM have different costs.
- Windows.
- Links with latency. Each peer is asked for four chunks at a time, which may
  leave a long-distance link idle between requests.
