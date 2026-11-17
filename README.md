<div align="center">

<img src="assets/logo.png" alt="mux" width="360">

**Copy large files and directories to many machines at once.**

[![CI][ci-badge]](../../actions/workflows/ci.yml)
[![Latest release][release-badge]](../../releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-green.svg)](LICENSE)

[How it works](docs/how-it-works.md) ·
[Architecture](docs/architecture.md) ·
[Protocol](docs/protocol.md) ·
[Performance](docs/performance.md) ·
[Contributing](CONTRIBUTING.md)

</div>

mux is a command-line tool and Rust library that copies large files and
directories, such as model weights, datasets, build outputs and disk images,
from one machine to many. Every machine that is downloading also serves the
parts it already has, so the tenth machine does not wait on the first. It is
a single binary: there is no tracker, scheduler or database to run. It builds
and is tested on Linux, macOS and Windows.

```
 direct download                    mux

      source                        source ─── A ─── C
   ┌──┬──┼──┬──┐                       │   ╲   │   ╱ │
   A  B  C  D  E                       B ───── D ─── E

 five machines share one uplink     every machine is also a source
```

> [!NOTE]
> **mux was called p2rent.** The latest release, 0.2.0, was published under
> that name, and its binaries and command are still `p2rent`. The first
> release named mux will be 0.3. Everything on this page is true of 0.2.0
> unless it is under [Not yet released](#not-yet-released).

## Example

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

The second machine to ask got part of the data from the first, which was
still seeding. (Ids are shortened and the timing line is left out.)

The ticket is all a machine needs. `--seed` keeps a machine serving after its
own download finishes. An interrupted download resumes: run the same command
again and only the missing chunks are fetched.

With release 0.2.0 the commands are the same, spelled `p2rent`:

| | Release 0.2.0 | Built from this source |
| --- | --- | --- |
| Command | `p2rent` | `mux` |
| Tickets | `p2rent://…` | `mux://…` |
| Key variable | `P2RENT_KEY` | `MUX_KEY` |
| Talks to | other 0.2.0 machines | other builds of this source |

## Features

- **Content addressed.** `seed` hashes files where they are with BLAKE3 and
  builds a manifest. The ticket carries the hash of that manifest, so a
  ticket pins exactly which bytes will arrive.
- **Verified before written.** Every chunk is checked against the manifest
  before it touches the disk. A peer that sends bad data is dropped and never
  contacted again.
- **Many sources.** Chunks are requested from several peers in parallel and
  offered to others as soon as they land. Peers find each other through the
  peers they already know.
- **No extra copies.** A seeder serves straight from the original files.
  Memory use does not grow with the size of what is sent.
- **Resumable.** There is no state file. Whatever is already at the
  destination is hashed, and only what does not match is fetched. The same
  step repairs a damaged copy.
- **Authenticated.** Every machine has an Ed25519 key. Connections are QUIC
  with TLS 1.3, authenticated in both directions against those keys.

### Not yet released

These are on the `main` branch and will be in 0.3. Release 0.2.0 does not
have them.

- **Only whole files appear.** A download is written to `<name>.mux-part`
  and renamed into place when every chunk of it has been verified. A file
  that was already there is replaced in one step or left alone.
- **The whole tree arrives.** Empty directories, symbolic links that stay
  inside the tree, permissions and modification times are kept. 0.2.0 keeps
  only the execute bit and leaves links out.
- **The same bytes are one swarm.** Names, permissions and times are kept
  apart from the content, so two machines holding the same bytes can supply
  each other whatever their copies are called.
- **The new name**, as in the table above. A machine that ran p2rent keeps
  its identity: its key is copied to the new configuration folder the first
  time mux runs.

## Installation

### Release 0.2.0

Binaries for Linux (x86_64, arm64), macOS (Intel, Apple silicon) and Windows
are attached to the [release](../../releases/tag/v0.2.0), with SHA-256
checksums. They are named `p2rent`. On Linux:

```console
$ curl -sSfL https://github.com/yuvichh/mux/releases/download/v0.2.0/p2rent-x86_64-linux.tar.gz | tar xz p2rent
$ sudo install p2rent /usr/local/bin/
```

The Linux binaries need glibc 2.17 or newer, which covers mainstream
distributions from the last ten years. The macOS binaries are not notarized:
if a browser download is blocked, run `xattr -d com.apple.quarantine p2rent`.

### From source

This builds `mux` from the `main` branch. It needs Rust 1.88 or newer.

```console
$ cargo install --git https://github.com/yuvichh/mux --locked cli
```

From a checkout, `cargo build --release` produces `target/release/mux`.

### Nix

```console
$ nix run github:yuvichh/mux -- --help
```

## Usage

| Command | Purpose |
| --- | --- |
| `mux seed <PATH>...` | Hash files or directories and serve them. Prints one ticket for each path on standard output. |
| `mux get <TICKET>` | Download into `--out`, which defaults to the current directory. |
| `mux id` | Print this machine's node id. |

Commonly used options:

| Option | Applies to | Meaning |
| --- | --- | --- |
| `--listen <ADDR>` | both | Address to bind. `seed` defaults to `0.0.0.0:4433`. |
| `--advertise <HOST[:PORT]>` | `seed` | Address to put in the ticket when the detected one is wrong, such as behind NAT or with a DNS name. |
| `--chunk-size <SIZE>` | `seed` | 4K to 16M. The default is 1M. |
| `--seed` | `get` | Keep serving after the download completes. |
| `--peer <NODE@HOST:PORT>` | `get` | Another peer to try, as well as those in the ticket. |
| `--key <FILE>` | all | Node key file. Also `MUX_KEY`. |

Run `mux <command> --help` for the full list.

## Security

- A ticket is a capability. Anyone who has it and can reach a peer can
  download the data. Treat a ticket for private data as a secret.
- Peers are identified by key, not by address. A downloader talks only to
  the node ids named in its ticket or introduced by those nodes.
- Integrity does not depend on trusting peers. All data is verified against
  the hash in the ticket.
- Links are never followed when seeding, and a manifest received from a peer
  is validated so that no path can leave the destination directory.

## Limitations

mux is not yet production software.

- Peers must be able to reach each other directly. There is no NAT traversal
  and no relay.
- There are no upload rate limits, and no access control beyond holding the
  ticket.
- If a seeded file changes on disk, its chunks stop verifying and
  downloaders drop that seeder. Seed again to publish the new contents.
- It is developed on macOS and Linux. Windows is covered by the test suite
  in CI and has not been used beyond that.

## Documentation

| Document | Covers |
| --- | --- |
| [How it works](docs/how-it-works.md) | An illustrated walk-through of a transfer. |
| [Architecture](docs/architecture.md) | The crates, the data model, storage and the download loop. |
| [Protocol](docs/protocol.md) | Tickets, the wire messages and the manifest format. |
| [Performance](docs/performance.md) | How speed is measured, the figures, and what was tried. |
| [Scope](docs/scope.md) | What the project is for, and what it will not do. |
| [Proofs](proofs/README.md) | Which parts are formally proven, and what the proofs rest on. |

## Repository layout

| Path | Holds |
| --- | --- |
| `crates/model` | The data model and the logic that is proven: hashes, manifests, chunk layout, the download schedule. No I/O, and no standard library. |
| `crates/engine` | What is the same for every place data goes: checking chunks, and committing a file when it is whole. |
| `crates/fs` | Local folders: hashing files, and reading and writing them. |
| `crates/web` | Web addresses: reading a file in parts, carrying on after a break, and noticing a file that changes. |
| `crates/peer` | Node identity, authenticated QUIC, the wire protocol and the swarm. |
| `crates/cli` | The `mux` command, and the `mux` library that gathers the others. |
| `benches` | Benchmarks. |
| `fuzz` | Fuzz targets for everything that parses bytes from another machine. |
| `proofs` | Lean 4 proofs of the logic in `crates/model`. |
| `xtask` | Development tasks, written in Rust. |

## Roadmap

mux is growing from a swarm distributor into a general tool and library for
moving and syncing data between any two places: folders, web URLs, other
machines and cloud storage. [docs/scope.md](docs/scope.md) describes where it
is going, and the [milestones](../../milestones) track the work. None of that
is in a release yet.

## Development

```console
$ just          # list the tasks
$ just ci       # every check CI runs
$ just all      # CI's checks, and the project's own analysis
$ just transfer # time a transfer over loopback
```

`nix develop` provides the toolchain.

## Contributing

[CONTRIBUTING.md](CONTRIBUTING.md) has the rules a change follows, and says
what each check does and which tools it needs. Work starts as an issue, and
one pull request closes it.

## License

mux is released under the [MIT license](LICENSE).

[ci-badge]: https://github.com/yuvichh/mux/actions/workflows/ci.yml/badge.svg
[release-badge]: https://img.shields.io/github/v/release/yuvichh/mux
