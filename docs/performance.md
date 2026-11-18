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
the 365 MiB/s that macOS reached at that size. With chunks sent one after
another ([#80](https://github.com/yuvichh/mux/issues/80)) the same macOS
transfer runs at 361 MiB/s and 7.7 CPU seconds per GiB.

What iroh adds to the binary and to the list of crates that ship is in the
pull request for #19.

### Over links like real ones

The rows of "Links like real ones" below, with quinn and with iroh, both
with the change of #80. MiB/s, one run each, 18 November 2026.

| Link, round trip | File | quinn | iroh |
| --- | ---: | ---: | ---: |
| 1 Gbit/s, 100 ms | 1 GiB | 91.3 | 90.4 |
| 1 Gbit/s, 20 ms | 1 GiB | 107.1 | 108.3 |
| 100 Mbit/s, 50 ms | 128 MiB | 10.7 | 10.6 |
| 100 Mbit/s, 200 ms | 128 MiB | 8.8 | 8.7 |
| 100 Mbit/s, 600 ms | 128 MiB | 5.5 | 5.3 |
| 0.5 Mbit/s, 50 ms | 4 MiB | 0.056 | 0.056 |
| 100 Mbit/s, 50 ms, 1% loss | 64 MiB | 1.03 | 0.59 |

Nothing is lost on a link without loss. With loss iroh's QUIC did worse in
each of three runs (0.57 to 0.68, against 0.78 to 1.06 in four runs with
quinn). Both are far below what the link carries, and what closes that gap
is the congestion control:
[#82](https://github.com/yuvichh/mux/issues/82) has the first figures for
both.

### Between two real machines

A laptop on Wi-Fi fetched 20 MiB from a machine on the same home network,
with a ticket that named the seeder by its key and gave no address. The
two met by way of a public relay, and iroh found the direct route between
them by itself within the first second: the log shows the connection move
from the relay to the seeder's address on the home network. It took 21.6
seconds, and the checksums were equal. `ssh` moves the same 20 MiB over
the same link in 21.3 seconds. That link carries 10 Mbit/s, which is what
the seeder's network card had agreed with its switch.

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

## Links like real ones

Loopback has no distance, no limit of speed and no loss. A real link has
all three, and some faults show only with them. So transfers are also timed
over a link made to behave like a real one.

One Linux machine holds two networks of its own, joined by a virtual cable.
They are made with `unshare`, which needs no root, and are gone when the
command ends. Each end of the cable is held to a rate by `tbf`, which also
cuts the bundles of packets the kernel hands over into single packets, and
is given a delay, a loss and a queue of a tenth of a second by `netem`. The
seeder or the web server is in one network and the downloader in the
other. The MTU is 1500.

`just far` is a shorter form of this, for the first look: the timed
transfer as it is, over the loopback of one such network. Both directions
then share a queue.

```console
$ just far 50 1000         # 100 ms there and back, at 1 Gbit/s
$ just far 25 100 1 64     # 50 ms, 100 Mbit/s, 1% loss, a 64 MiB file
```

### A fixed number of requests

Until [#80](https://github.com/yuvichh/mux/issues/80) a machine was asked
for four chunks at a time. A request takes a round trip before its first
byte comes back, so a fast link to a far machine carried nothing for most
of each round trip. On a slow link the four shared it, none finished
within the time a transfer waits for a chunk, and the transfer gave up.

Now the number follows how long a chunk takes to come: see "Transfer" in
[architecture.md](architecture.md#transfer).

MiB/s for the whole command, one run each, on an Intel Core i5-10200H with
the files on tmpfs, 18 November 2026. curl and aria2c fetch from nginx, and
so does `mux cp`. `mux get` fetches from `mux seed`.

| Link, round trip | File | curl | aria2c, 4 connections | `mux cp` from the web | `mux get` before | `mux get` after |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 Gbit/s, 100 ms | 1 GiB | 26.5 | 95.7 | 80.1 | 33.8 | **91.3** |
| 1 Gbit/s, 20 ms | 1 GiB | 111.1 | 106.1 | 97.2 | 108.3 | 107.1 |
| 100 Mbit/s, 50 ms | 128 MiB | 11.0 | 10.5 | 9.5 | 10.7 | 10.7 |
| 100 Mbit/s, 200 ms | 128 MiB | 9.3 | 9.2 | 5.7 | 8.7 | 8.8 |
| 100 Mbit/s, 600 ms | 128 MiB | 5.2 | 3.4 | 3.6 | 3.8 | **5.5** |
| 0.5 Mbit/s, 50 ms | 4 MiB | 0.060 | | 0.034 | gives up after 60 s | **0.056** |
| 100 Mbit/s, 50 ms, 1% loss | 64 MiB | 0.48 | 2.17 | 1.86 | 0.84 | 1.03 |

1 Gbit/s is 119 MiB/s, 100 Mbit/s is 11.9 and 0.5 Mbit/s is 0.060.

- **Far and fast:** 2.7 times the rate. Four chunks of 1 MiB in a round
  trip of 100 ms can be 40 MiB/s at the very most.
- **Slow:** it works where it could not. One chunk is asked for at a time,
  and it is there in 17 seconds.
- **Near, or not fast:** no change, as there should be none.
- **Loopback**, where the CPU is the limit: 6.75 CPU seconds per GiB
  before and 5.55 after, with the builds taken in turn three times. The
  rate is the same within what that benchmark can tell apart: 423 to
  443 MiB/s before, 407 to 468 after.

The CPU is saved by the order in which a seeder sends. With every chunk
that was asked for sharing the connection a slice at a time, as a QUIC
connection does unless told otherwise, 64 requests in flight cost 8.3 CPU
seconds per GiB: each packet woke another reader. A seeder now sends the
chunks one after another, in the order they were asked for, by giving each
a lower priority than the one before.

### What these runs show and this did not change

- **Loss.** With 1% of packets lost, one connection is held back by its
  congestion control, which halves its rate at each loss. `mux get` is one
  QUIC connection and does twice as well as one TCP connection, but tools
  that open four connections get four times one. A congestion control that
  does not take every loss for congestion (BBR) is the thing to measure:
  [#82](https://github.com/yuvichh/mux/issues/82).
- **`mux cp` from the web, far away.** At 200 ms it reached 62% of curl:
  see "A reader left open" below, and
  [#84](https://github.com/yuvichh/mux/issues/84).
- **curl at 1 Gbit/s and 100 ms** is held to 26.5 MiB/s by the size of the
  receive window that Linux gives one TCP connection. It is the figure for
  one connection with nothing tuned, not a limit of TCP.
- **600 ms.** Every tool spends most of a 128 MiB file getting up to speed.
- A transfer counted progress in whole chunks. With no chunk stored for the
  stall timeout it was taken for stalled, even while bytes arrived. `mux cp`
  from the web over 0.5 Mbit/s gave up on an 8 MiB file after 100 seconds
  for that reason. Progress is now counted in bytes
  ([#83](https://github.com/yuvichh/mux/issues/83)): the same copy takes
  145 seconds, and `mux get` of the same file 142, at 93% of what the link
  carries. Loopback costs the same CPU as before within a hundredth (5.59
  CPU seconds per GiB against 5.55), and the first row of the table reads
  91.5 MiB/s.

### A reader left open

A request to a web server is for the rest of the file from where it
starts. A stream that comes to a part another stream has fetched starts a
request elsewhere. The reader it left was kept open, in case somebody read
on from it. Nobody did, and the server went on sending into it until the
connection's window was full, over the same link the other streams were
using.

nginx was made to log each request and the bytes it sent. A 128 MiB file
over 100 Mbit/s at 200 ms:

| | Requests | Bytes the server sent | Time |
| --- | ---: | ---: | ---: |
| curl | 1 | 128.0 MiB | 13.2 s |
| `mux cp --streams 1` | 2 | 128.0 MiB | 13.6 s |
| `mux cp`, four streams, before | 13 | 201.0 MiB | 18.1 s |
| `mux cp`, four streams, after | 13 | 154.7 MiB | 17.5 s |

A reader is now closed as soon as the stream that was reading from it goes
elsewhere. `mux cp` with four streams, MiB/s, the median of three runs
with the builds taken in turn, and the range:

| Link, round trip | File | Before | After | curl |
| --- | ---: | ---: | ---: | ---: |
| 1 Gbit/s, 20 ms | 1 GiB | 98.8 (95.4 to 102.7) | 107.1 (107.0 to 108.0) | 111.1 |
| 1 Gbit/s, 100 ms | 1 GiB | 69.6 (66.5 to 79.4) | 83.4 (78.7 to 83.5) | 26.5 |
| 100 Mbit/s, 50 ms | 128 MiB | 9.2 (9.2 to 9.4) | 9.9 (9.8 to 9.9) | 11.0 |
| 100 Mbit/s, 200 ms | 128 MiB | 6.6 (6.3 to 6.7) | 7.6 (7.5 to 7.8) | 9.3 |
| 100 Mbit/s, 50 ms, 1% loss | 64 MiB | 1.91 | 1.91 | 0.48 |

What is left is the number of requests. Near the end of a copy a stream
that has finished its part takes half of what another has left, again and
again, and each time that is a new connection that starts slowly. One
stream is as fast as curl on these links, and four are still slower where
one connection can fill the link by itself. That is the rest of #84.

## Sending only what changed

A destination that holds an older version of a file should not have to
fetch the whole of the newer one
([#74](https://github.com/yuvichh/mux/issues/74)). `just delta` compares
three ways of finding what it can keep, on three kinds of change:

- **Same place.** The new file is cut into chunks of one size, and a chunk
  is kept if the old file has the same bytes at the same place. This is
  what a transfer does today when the digests are known.
- **Search.** The same chunks, and a chunk is kept if the old file has its
  bytes anywhere. The old file is gone through once with a hash that rolls
  from each byte to the next, as `rsync` and `zsync` do.
- **Cut by content.** Both files are cut where their bytes say, so that a
  cut falls in the same place after something is inserted before it.

What would still have to be fetched, for a file of 256 MiB in chunks of
1 MiB, on the Linux machine in the table above:

| Change | Same place | Search | Cut by content, about 1 MiB | Cut by content, about 64 KiB |
| --- | ---: | ---: | ---: | ---: |
| Grown by 1% (2.6 MiB) | 2.6 MiB | 2.6 MiB | 4.8 MiB | 2.6 MiB |
| 4 KiB changed in the middle | 1.0 MiB | 1.0 MiB | 0.7 MiB | 0.05 MiB |
| 4 KiB inserted a third of the way in | 171 MiB | 1.0 MiB | 1.6 MiB | 0.3 MiB |

For a file of 1 GiB the last row is 683 MiB, 1.0 MiB, 1.5 MiB and 0.05 MiB.

What each costs:

| | Same place | Search | Cut by content, about 1 MiB | About 64 KiB |
| --- | ---: | ---: | ---: | ---: |
| Going through the old file | nothing more | 320 MiB/s | 1,110 MiB/s | |
| What the source says of each GiB, beside the bytes | 32 KiB | 40 KiB | 36 KiB | 576 KiB |
| Chunks of one size | yes | yes | no | no |

BLAKE3 goes through the same file at 3,800 MiB/s on this machine. The
figure for the search is all of the destination's work: hashing the chunks
it looks for, rolling through the old file, and checking each chunk it
finds with BLAKE3.

**The choice is the search.**

- Today's way fails on the commonest change to a large file that is not
  an append: something put in or taken out near its start. Everything after
  it has moved, and all of it is fetched.
- The search fetches one chunk and what is new, in all three cases.
- Cutting by content does about as well at the same size of chunk, and
  better at a smaller one. But it gives up chunks of one size. The layout
  that says where each chunk lies is proven for chunks of one size, and a
  source whose bytes are not known beforehand, such as a web server, can
  only be asked for chunks of one size. Small chunks also make the list of
  digests sixteen times as long.
- The search costs a pass over the old file at 320 MiB/s, and only for a
  file that has changed. That is 2.7 Gbit/s, faster than the links on which
  it saves anything.

Two things the measurement changed:

| Change | Result |
| --- | --- |
| The checksum of `rsync`: two sums of 16 bits | First version. In 256 MiB it matched by chance 18 times, each ruled out by hashing a chunk. The number grows with the size of the file times its number of chunks: millions for a file of 100 GiB. |
| A rolling hash of 64 bits: each byte times a power of an odd number | Kept. No match by chance in 256 MiB or in 1 GiB, at the cost of two multiplications for each byte. |
| Asking a table at every byte whether a chunk starts there | First version: 170 MiB/s on a laptop. |
| A bit for each of a million values of the hash, and the table asked only where the bit is set | Kept: 570 MiB/s on the same laptop. |

A file made to collide could still make the hash match everywhere, so the
search will stop after a set number of matches that were not the chunk,
and fall back to comparing in place.

The rolling loop is written in safe Rust. It is the first candidate for
the fast path below, once the search is part of a transfer and there is a
link fast enough to be held up by it.

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
- A real link with a real router, to check the made-up one against.
- Many machines fetching from one, and one fetching from many, over links
  like real ones.
- Web downloads over a real network, and over HTTPS.
- The search for moved chunks on files that are not random: text, images of
  disks with long runs of zeros, archives.
- Where the system time of several web streams goes.
