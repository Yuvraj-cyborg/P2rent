# Performance notes

What has been measured, what helped and what did not, so the same ground is
not covered twice. Reproduce any of it with `just bench`.

All figures are loopback transfers of a 1 GiB file from one seeder unless
stated. Loopback has no loss, no latency and no network card, so they show
CPU cost per byte, not what a real network delivers. They come from one
laptop (Apple M4) and a Linux container running on it; treat differences
under about 15% as noise.

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

## Method notes

- Compare CPU seconds per GiB first. Wall time on a laptop moves with disk
  state and temperature; on a nearly full SSD it ranged from 85 to 360 MiB/s
  between identical runs. Put `P2RENT_BENCH_DIR` on a RAM disk or tmpfs.
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
