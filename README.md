<p align="center">
  <img src="assets/logo.png" alt="mux" width="360">
</p>

<p align="center">
  Swarm distribution of large artifacts over QUIC.
</p>

<p align="center">
  <a href="https://github.com/yuvichh/mux/actions/workflows/ci.yml"><img src="https://github.com/yuvichh/mux/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-green.svg" alt="MIT license"></a>
</p>

mux copies large files and directories (model weights, datasets, build
outputs, VM images) to many machines at once. Every machine that is
downloading also serves the parts it already has, so the tenth machine is not
waiting on the first. It is a single binary with no tracker, scheduler or
database to run.

```
 direct download                    mux

      source                        source ─── A ─── C
   ┌──┬──┼──┬──┐                       │   ╲   │   ╱ │
   A  B  C  D  E                       B ───── D ─── E

 five machines share one uplink     every machine is also a source
```

## Quick start

On the machine that has the data:

```console
$ mux seed ./weights
seeding  weights  (400.68 MiB, 402 chunks)
mux://450b9c6b…6d2bfc54?peer=35964d95…f14dd0e7@10.0.0.5:4433
```

On every machine that needs it:

```console
$ mux get 'mux://450b9c6b…6d2bfc54?peer=35964d95…f14dd0e7@10.0.0.5:4433' --out /models --seed
saved    /models/weights  (400.68 MiB)
fetched  400.68 MiB from 2 peers
         35964d95  219.68 MiB
         aec1c5f2  181.00 MiB
```

Here the second machine to ask got part of the data from the first one, which
was still seeding. (Ids are shortened and the timing line is omitted.)

The ticket is the only thing a machine needs. `--seed` keeps the machine
serving after its own download finishes. Interrupted downloads resume: run the
same command again and only the missing chunks are fetched. A file appears
under its own name only when it is complete; until then it is
`<name>.mux-part`.

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
  write each file once, beside its destination, and rename it into place.
  Memory use does not grow with artifact size.
- **The whole tree.** Empty directories, symbolic links that stay inside the
  tree, permissions and modification times arrive as they were. The same
  bytes are one swarm whatever they are called: names, permissions and times
  are kept apart from the content.
- **Authenticated.** Every node has an Ed25519 key. Connections are QUIC with
  TLS 1.3, authenticated in both directions against those keys.

More detail: [illustrated walk-through](docs/how-it-works.md) ·
[architecture](docs/architecture.md) · [protocol](docs/protocol.md) ·
[performance notes](docs/performance.md)

## Install

mux was called p2rent up to release 0.2.0. The first release under the new
name will be 0.3. Until then, build from source to get `mux`:

```console
$ cargo install --git https://github.com/yuvichh/mux --locked cli
```

With Nix:

```console
$ nix run github:yuvichh/mux -- --help
```

From a checkout, `cargo build --release` produces `target/release/mux`.
Building needs Rust 1.88 or newer.

The binaries attached to
[release 0.2.0](https://github.com/yuvichh/mux/releases/tag/v0.2.0),
and the command inside them, still carry the old name. They do not talk to a
build from this source: the protocol identifier and the ticket format changed
with the name. To install them on Linux:

```console
$ curl -sSfL https://github.com/yuvichh/mux/releases/download/v0.2.0/p2rent-x86_64-linux.tar.gz | tar xz p2rent
$ sudo install p2rent /usr/local/bin/
```

There are builds for Linux (x86_64, arm64), macOS (Intel, Apple silicon) and
Windows, with SHA-256 checksums. The Linux binaries need glibc 2.17 or newer,
which covers mainstream distributions from the last ten years. The macOS
binaries are not notarized: if a browser download is blocked, run
`xattr -d com.apple.quarantine p2rent`.

A machine that ran p2rent keeps its identity under mux: the key in the old
configuration folder is copied to the new one the first time mux runs.

## Usage

| Command | Purpose |
| --- | --- |
| `mux seed <PATH>...` | Hash files or directories and serve them. Prints one ticket per path on stdout. |
| `mux get <TICKET>` | Download into `--out` (default: current directory). |
| `mux id` | Print this machine's node id. |

Commonly used options:

| Option | Applies to | Meaning |
| --- | --- | --- |
| `--listen <ADDR>` | both | Address to bind. `seed` defaults to `0.0.0.0:4433`. |
| `--advertise <HOST[:PORT]>` | `seed` | Address to put in the ticket when the detected one is wrong, such as behind NAT or with a DNS name. |
| `--chunk-size <SIZE>` | `seed` | 4K to 16M, default 1M. |
| `--seed` | `get` | Keep serving after the download completes. |
| `--peer <NODE@HOST:PORT>` | `get` | Extra peer to try, in addition to those in the ticket. |
| `--key <FILE>` | all | Node key file. Also `MUX_KEY`. |

Run `mux <command> --help` for the full list.

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

This is a rewrite and is not yet production software.

- Developed and tested on macOS. CI runs the test suite on Linux, macOS and
  Windows; Windows has not been used beyond that.
- Peers must be able to reach each other directly. There is no NAT traversal
  or relay yet.
- There are no upload rate limits or access controls beyond possession of
  the ticket.
- If a seeded file changes on disk, its chunks stop verifying and downloaders
  will drop that seeder. Seed again to publish the new contents.

## Direction

mux is growing from a swarm distributor into a general tool and library
for moving and syncing data between any two places: folders, web URLs, other
machines and cloud storage. [docs/scope.md](docs/scope.md) describes where it
is going, the [milestones](https://github.com/yuvichh/mux/milestones)
track the work, and [CONTRIBUTING.md](CONTRIBUTING.md) has the rules a change
follows.

## Development

```console
$ just          # list tasks
$ just ci       # every check CI runs
$ just all      # CI's checks plus the project's own analysis
$ just transfer # time a loopback transfer
```

`nix develop` provides the toolchain. [CONTRIBUTING.md](CONTRIBUTING.md)
lists what each check does and the tools it needs.

## License

MIT. See [LICENSE](LICENSE).
