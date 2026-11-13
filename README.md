<p align="center">
  <img src="assets/logo.png" alt="p2rent" width="380">
</p>

<p align="center">
  Swarm distribution of large artifacts over QUIC.
</p>

<p align="center">
  <a href="https://github.com/Yuvraj-cyborg/P2rent/actions/workflows/ci.yml"><img src="https://github.com/Yuvraj-cyborg/P2rent/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-green.svg" alt="MIT license"></a>
</p>

p2rent copies large files and directories (model weights, datasets, build
outputs, VM images) to many machines at once. Every machine that is
downloading also serves the parts it already has, so the tenth machine is not
waiting on the first. It is a single binary with no tracker, scheduler or
database to run.

## Quick start

On the machine that has the data:

```console
$ p2rent seed ./weights
seeding  weights  (400.68 MiB, 402 chunks)
p2rent://450b9c6b…6d2bfc54?peer=35964d95…f14dd0e7@10.0.0.5:4433
```

On every machine that needs it:

```console
$ p2rent get 'p2rent://450b9c6b…6d2bfc54?peer=35964d95…f14dd0e7@10.0.0.5:4433' --out /models --seed
saved    /models/weights  (400.68 MiB)
fetched  400.68 MiB from 2 peers
         35964d95  219.68 MiB
         aec1c5f2  181.00 MiB
```

Here the second machine to ask got part of the data from the first one, which
was still seeding. (Ids are shortened and the timing line is omitted.)

The ticket is the only thing a machine needs. `--seed` keeps the machine
serving after its own download finishes. Interrupted downloads resume: run the
same command again and only the missing chunks are fetched.

## How it works

- **Content addressed.** `seed` hashes files in place with BLAKE3 and builds a
  manifest. The ticket carries the hash of that manifest, so a ticket pins
  exactly which bytes will arrive.
- **Verified before written.** Each chunk is checked against the manifest
  before it touches the disk. A peer that sends bad data is dropped and never
  contacted again.
- **Many sources.** Chunks are requested from several peers in parallel and
  offered to others as soon as they land. Peers find each other through the
  peers they already know.
- **No copies.** Seeders serve straight from the original files; downloaders
  write straight into the destination. Memory use does not grow with artifact
  size.
- **Authenticated.** Every node has an Ed25519 key. Connections are QUIC with
  TLS 1.3, authenticated in both directions against those keys.

More detail: [architecture](docs/architecture.md) · [protocol](docs/protocol.md) ·
[performance notes](docs/performance.md)

## Install

```console
$ cargo install --git https://github.com/Yuvraj-cyborg/P2rent --locked
```

With Nix:

```console
$ nix run github:Yuvraj-cyborg/P2rent -- --help
```

From a checkout, `cargo build --release` produces `target/release/p2rent`.
Building needs Rust 1.88 or newer.

## Usage

| Command | Purpose |
| --- | --- |
| `p2rent seed <PATH>...` | Hash files or directories and serve them. Prints one ticket per path on stdout. |
| `p2rent get <TICKET>` | Download into `--out` (default: current directory). |
| `p2rent id` | Print this machine's node id. |

Commonly used options:

| Option | Applies to | Meaning |
| --- | --- | --- |
| `--listen <ADDR>` | both | Address to bind. `seed` defaults to `0.0.0.0:4433`. |
| `--advertise <HOST[:PORT]>` | `seed` | Address to put in the ticket when the detected one is wrong, such as behind NAT or with a DNS name. |
| `--chunk-size <SIZE>` | `seed` | 4K to 16M, default 1M. |
| `--seed` | `get` | Keep serving after the download completes. |
| `--peer <NODE@HOST:PORT>` | `get` | Extra peer to try, in addition to those in the ticket. |
| `--key <FILE>` | all | Node key file. Also `P2RENT_KEY`. |

Run `p2rent <command> --help` for the full list.

## Security model

- A ticket is a capability: anyone who has it, and can reach a peer, can
  download the artifact. Treat tickets for private data as secrets.
- Peers are identified by key, not address. A downloader only talks to the
  node ids named in its ticket or introduced by those nodes.
- Integrity does not depend on trusting peers: all data is verified against
  the ticket's hash.
- Symlinks are not followed when seeding, and manifests received from peers
  are validated so that no path can leave the destination directory.

## Status

Version 0.2 is a rewrite and is not yet production software.

- Developed and tested on macOS. CI runs the test suite on Linux, macOS and
  Windows; Windows has not been used beyond that.
- Peers must be able to reach each other directly. There is no NAT traversal
  or relay yet.
- There are no upload rate limits or access controls beyond possession of
  the ticket.
- If a seeded file changes on disk, its chunks stop verifying and downloaders
  will drop that seeder. Seed again to publish the new contents.

## Roadmap

1. Origin fallback: seed from, and fall back to, an HTTP, S3 or Hugging Face
   URL, so a swarm is never slower than a direct download.
2. LAN discovery, so machines on one network need no ticket peers.
3. Private swarms gated by a shared key or a node allowlist.
4. A long-running daemon with a disk quota, rate limits and metrics.

## Development

```console
$ just          # list tasks
$ just ci       # format check, clippy, tests
$ just demo     # three nodes on this machine
```

`nix develop` provides the toolchain. Without `just`, the same checks are
`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and
`cargo test`.

## License

MIT. See [LICENSE](LICENSE).
