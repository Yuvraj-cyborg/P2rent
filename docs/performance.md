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
| `build_artifact` | Check, encode and name a manifest of 1000 files and 10,000 chunks | 5,425,852 |
| `decode_artifact` | Check and decode the same manifest as bytes from a peer | 5,012,817 |
| `locate_every_chunk` | Find the file, offset and length of each of 10,000 chunks | 1,493,611 |
| `mark_and_count` | Set every third bit of a 10,000-bit bitfield, then count them | 67,093 |
| `claim_every_chunk` | Claim and complete each of 10,000 chunks | 581,002 |
| `read_chunks` | Read 16 chunks of 1 MiB from a file | 908,460 |
| `store_chunks` | Check 16 chunks of 1 MiB against the manifest, write them to a part file, then flush it and rename it into place | 41,929,685 |

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
| The same machine, with connections made by iroh, 18 November 2026 | 472 MiB/s | 6.52 | 10,592 MiB/s |

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
([#14](https://github.com/yuvichh/mux/issues/14)). Measured on Linux
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

## Manifest format 1

The manifest gained a format version, entries for directories and links,
permissions and times, and a content id kept apart from the artifact id
([#15](https://github.com/yuvichh/mux/issues/15)). Instructions,
before and after:

| Benchmark | Before | After |
| --- | ---: | ---: |
| `build_artifact` | 5,191,641 | 5,422,858 (+4.5%) |
| `decode_artifact` | 6,040,211 | 5,009,843 (-17.1%) |

Building checks more than it did: every entry's directory has to be listed,
and there are two identifiers to derive. Decoding got cheaper, although it
checks more too, because of one choice that was measured first.

| Change | Result |
| --- | --- |
| Derive the artifact id from the whole encoded manifest, and the content id from the content | Tried first. The content, which is nearly all of a manifest, is hashed twice. |
| Derive the artifact id from the content id and the metadata | Kept. The content is hashed once, and the artifact id still pins it. |

No other benchmark moved.

## Places and the engine

Storage was split in two
([#17](https://github.com/yuvichh/mux/issues/17)). A place, such as a local
folder, only moves bytes. The engine's store checks each chunk and decides
when a file is whole, for whatever place it is given. A chunk goes through
the same steps as before, and they cost the same:

| Measurement | Before | After |
| --- | ---: | ---: |
| `read_chunks`, instructions | 909,183 | 908,460 (-0.1%) |
| `store_chunks`, instructions | 41,931,238 | 41,929,685 (0.0%) |
| One 1 GiB file, throughput | 423 MiB/s | 416 MiB/s |
| One 1 GiB file, CPU seconds per GiB | 6.77 | 6.81 (+0.6%) |
| 1 GiB in 2000 files, throughput | 438 MiB/s | 430 MiB/s |
| 1 GiB in 2000 files, CPU seconds per GiB | 6.88 | 6.95 (+1.0%) |

The timed rows are the median of five loopback runs on the Linux machine in
the table above. The runs of one build differ from each other by more than
the two builds differ, so the timed rows show no change.

The instruction benchmarks run the store's steps on one thread, through the
folder. The engine runs the same steps on a thread that may wait.

One design was measured and dropped:

| Change | Result |
| --- | --- |
| Every operation on a place returns a future, and a folder does each one on a thread of its own choosing | Tried first. Checking a chunk and writing it became two hand-offs between threads where there had been one, and CPU seconds per GiB rose by 1.3% (6.86 against 6.77). |
| Reading returns a future. Writing is a plain call, which the engine makes on the thread that has just checked the chunk | Kept. One hand-off for each chunk, as before. |

The loop that fetches chunks then moved from `peer` to the engine, where it
works for any supplier and not only for another machine. It gained four
rules that keep a transfer from stalling (see
[architecture.md](architecture.md#transfer)), and each chunk that arrives is
now stored and counted in a task of its own. None of it shows in the
figures:

| Measurement | Before | After |
| --- | ---: | ---: |
| All eight instruction benchmarks | | identical, to the instruction |
| One 1 GiB file | 414 MiB/s, 6.83 CPU s/GiB | 386 MiB/s, 6.80 CPU s/GiB |
| 1 GiB in 2000 files | 431 MiB/s, 6.88 CPU s/GiB | 434 MiB/s, 6.95 CPU s/GiB |
| One 1 GiB file from two seeders | 654 MiB/s, 6.68 CPU s/GiB | 646 MiB/s, 6.65 CPU s/GiB |

## Connections by iroh

`peer` made its connections with quinn until
[#19](https://github.com/yuvichh/mux/issues/19) moved it to
[iroh](https://github.com/n0-computer/iroh), which reaches machines behind
routers and has a QUIC of its own. The same loopback transfer, before and
after:

| Machine | quinn | iroh |
| --- | ---: | ---: |
| Linux, Intel Core i5-10200H, 1 GiB on tmpfs, median of five | 441 MiB/s, 6.82 CPU s/GiB | 472 MiB/s, 6.52 CPU s/GiB |
| macOS, Apple M4, 512 MiB, median of five | about 1,350 MiB/s (measured when large datagrams went in) | 328 MiB/s, 9.84 CPU s/GiB |

On Linux nothing was lost. On macOS loopback the rate fell to a quarter,
and the reason is the size of a datagram. With quinn a datagram grew to
8952 bytes where the route could carry it, and on macOS, which pays for
each packet with a system call, that was worth nearly four times the
throughput. An iroh endpoint accepts datagrams of up to 1472 bytes and has
no setting for more.

This is the cost on a route with a large MTU, which is loopback and a
network of jumbo frames. On the usual 1500-byte network a datagram could
not grow past 1472 bytes before either, and the figure to compare with is
the 365 MiB/s that macOS reached at that size.

What iroh adds to the binary and to the list of crates that ship is in the
pull request for #19.

One transfer was made between two machines on different networks, each
behind a router, with a ticket that named the seeder by its key and gave no
address: 20 MiB in 22.0 seconds, at the 1 MiB/s that link gives, and the
checksums were equal.

## Web downloads

`just fetch URL` fetches one web address into a folder, three times, and
reports throughput and CPU seconds per GiB
([#18](https://github.com/yuvichh/mux/issues/18)). The server is whatever
answers at the address, so other tools can be timed fetching the same file
from the same server:

```console
$ just fetch http://127.0.0.1:8098/big.bin        # four streams
$ just fetch http://127.0.0.1:8098/big.bin 1      # one stream
$ time curl -s -o big.bin http://127.0.0.1:8098/big.bin
$ time aria2c -q -x 4 -s 4 --file-allocation=none -o big.bin http://127.0.0.1:8098/big.bin
```

`benches/web.rs` lists the settings.

The figures are for a 1 GiB file of random bytes, served by nginx 1.30
(`sendfile on`) on the Linux machine in the table above, with the file and
the copy both on tmpfs. Each is the median of five runs on 18 November
2026.

| Tool | Connections | Throughput | CPU seconds per GiB |
| --- | ---: | ---: | ---: |
| mux | 4 | 2,746 MiB/s | 1.30 |
| mux | 1 | 1,532 MiB/s | 0.71 |
| curl 8.22 | 1 | 2,301 MiB/s | 0.39 |
| aria2c 1.37 | 4 | 1,499 MiB/s | 0.59 |
| aria2c 1.37 | 1 | 1,134 MiB/s | 0.82 |

How to read them:

- This is loopback, so the figures show CPU cost per byte. On a real
  network the network is the limit: 2,746 MiB/s is 23 Gbit/s.
- mux hashes every byte as it stores it and notes the digest, so that the
  copy can be carried on after an interruption and seeded when it is whole.
  `curl` and `aria2c` were run as above and check nothing.
- Over one stream mux is slower than `curl`. A chunk is hashed and written
  before the next one is read, so the two do not overlap.
- Four streams cost more CPU for each byte than one, 1.30 against 0.71, and
  most of the difference is system time (0.82 against 0.37). Where it goes
  is not known: it needs a profiler that can see into the kernel, which
  this machine does not have.

One change was measured, before a copy was made to start at the first chunk
of a file, which moved neither figure:

| Change | Result |
| --- | --- |
| What arrives is appended to a buffer, and a chunk is cut from its front | First version. 2,535 MiB/s and 1.35 CPU s/GiB over four streams, 1,392 MiB/s and 0.75 over one. |
| Each byte is copied once, from where it arrived into a buffer of the chunk's size | Kept. 2,758 MiB/s and 1.28 over four streams, 1,495 MiB/s and 0.72 over one. |

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
- How much of a transfer between machines behind routers goes direct, and
  how much by way of a relay.
- x86 servers, where BLAKE3 and AES-GCM have different costs.
- Windows.
- Links with latency. Each peer is asked for four chunks at a time, which may
  leave a long-distance link idle between requests.
- Web downloads over a real network, and over HTTPS.
- Where the system time of several web streams goes.
