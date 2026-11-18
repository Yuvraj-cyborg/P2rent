<div align="center">

<img src="assets/logo.png" alt="mux" width="360">

**Copy and sync data between folders, the web and other machines.**

[![CI][ci-badge]](../../actions/workflows/ci.yml)
[![Latest release][release-badge]](../../releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-green.svg)](LICENSE)

[Commands](docs/commands.md) ·
[How it works](docs/how-it-works.md) ·
[Architecture](docs/architecture.md) ·
[Protocol](docs/protocol.md) ·
[Performance](docs/performance.md) ·
[Changelog](CHANGELOG.md)

</div>

mux is one command-line tool and one Rust library for moving data from one
place to another. A place is a folder, a web address or another machine.
The commands are the same whatever the two places are, and so is what they
promise: a file has its name only when it is whole, a copy that was
interrupted carries on, and what cannot be checked is not called checked.
It is a single binary. It builds and is tested on Linux, macOS and Windows.

```console
$ mux cp https://example.org/big.iso ./downloads/     # from the web
$ mux cp ./weights /mnt/backup/                       # a folder
$ mux cp ./weights peer:                              # offer it to other machines
$ mux cp 'mux://450b9c6b…' /models/                   # fetch it on another machine
$ mux sync ./weights /mnt/backup/weights --delete     # only what changed
$ mux probe https://example.org/big.iso               # what is there
```

> [!NOTE]
> **mux was called p2rent** up to release 0.2.0. Release 0.3 is the first
> under the new name, and it does not talk to 0.2 machines:
> [what to know when moving](#coming-from-p2rent-02).

## Examples

### From the web

```console
$ mux cp https://example.org/big.iso ./downloads/
./downloads/big.iso	1288490188	consistent
```

The file is read over several connections. Run the same command again
after an interruption and it carries on where it stopped. If the file
changes on the server meanwhile, the copy stops with an error and keeps
nothing of it. With `--hash sha256:…` the copy is verified, and
`--mirror URL` reads the same file from a second server at once.

The line on standard output is what was written, its size in bytes, and
how far it was checked: `verified` against a digest, or `consistent`,
which means no digest was known and the source stayed what it was.

### Between machines

On the machine that has the data:

```console
$ mux cp ./weights peer:
seeding  weights  (400.68 MiB, 402 chunks)
mux://450b9c6b…6d2bfc54?peer=35964d95…f14dd0e7@https://euc1-1.relay.n0.iroh.link./
```

On every machine that needs it:

```console
$ mux cp 'mux://450b9c6b…6d2bfc54?peer=35964d95…f14dd0e7@https://euc1-1.relay.n0.iroh.link./' /models/ --seed
/models/weights	420143104	verified
```

The ticket is all a machine needs. It names the content by its hash and
the machine by its key, with the relay that machine can be reached through
(ids are shortened here). Every chunk is checked before it is written,
whoever sent it. With `--seed`
a machine goes on offering what it fetched, so the tenth machine does not
wait on the first:

```
 direct download                    mux

      source                        source ─── A ─── C
   ┌──┬──┼──┬──┐                       │   ╲   │   ╱ │
   A  B  C  D  E                       B ───── D ─── E

 five machines share one uplink     every machine is also a source
```

Two machines behind routers reach each other directly where their routers
allow it, and through a relay where they do not.

### The web and other machines at once

The first machine copies from the web and goes on offering the file:

```console
$ mux cp https://example.org/big.iso ./downloads/ --seed
./downloads/big.iso	1288490188	consistent
mux://450b9c6b…6d2bfc54?peer=35964d95…f14dd0e7@https://euc1-1.relay.n0.iroh.link./&url=https://example.org/big.iso
```

Every other machine uses that ticket. It reads from the machines that have
the file and from the web address in one transfer, and checks every chunk
against the first machine's copy.

### Keeping a folder the same as another

```console
$ mux sync ./weights /mnt/backup/weights --delete
remove	old-notes.txt
add	run-7/config.json
replace	run-7/model.bin
```

Only what the destination lacks or has in another version is read. Nothing
is removed without `--delete`. What is replaced or removed goes to a trash
folder first, and a sync that would remove more than half of a folder
stops before it changes anything.

### Looking before copying

```console
$ mux probe https://example.org/big.iso
name     big.iso
kind     file
size     1288490188 bytes (1.20 GiB)
files    1
checked  consistent, if it is copied
parts    read several at once
```

`--dry-run` on `cp` and `sync` says what would be done and does none of it.
`--json` on any command writes one JSON object on each line for a program
to read, and the [exit codes](docs/commands.md#exit-codes) say whether
running a command again is worth it.

## What it promises

- **A file has its name only when it is whole.** It is written under a
  name ending `.mux-part` and renamed when every byte is in place. However
  the program stops, there is never a file with holes under a name
  somebody would open.
- **A copy carries on.** Run the same command again. What is there is
  checked, and only what is missing is fetched. There is no state file.
- **It says how far it checked.** `verified` means every byte was compared
  with a digest known beforehand. `consistent` means there was none. It
  never claims more.
- **Between machines, nobody has to be trusted.** Every chunk is compared
  with the digest in the manifest before it touches the disk, and a
  machine that sends anything else is dropped.
- **The whole tree arrives:** empty folders, links that stay inside the
  tree, permissions and times.
- **A sync does not destroy data.** See above.
- **Several places feed one transfer:** any number of machines, and web
  addresses beside them.

The rules behind some of these are proven: which chunk is fetched next,
where a path may point, what a sync does to each path.
[proofs/README.md](proofs/README.md) says what is proven and what the
proofs rest on.

## Speed

Measured over links shaped to behave like real ones.
[docs/performance.md](docs/performance.md) has the method and every figure.

- From a web server, `mux cp` is level with curl where one connection
  fills the link, a few percent faster far from the server, and several
  times faster where the link is fast and far or loses packets.
- Between machines, a transfer asks for as many chunks at once as the
  route has room for.

## Installation

### A release

Binaries for Linux (x86_64, arm64), macOS (Intel, Apple silicon) and
Windows are attached to each [release](../../releases), with SHA-256
checksums. On Linux:

```console
$ curl -sSfL https://github.com/yuvichh/mux/releases/latest/download/mux-x86_64-linux.tar.gz | tar xz mux
$ sudo install mux /usr/local/bin/
```

The Linux binaries need glibc 2.17 or newer, which covers mainstream
distributions from the last ten years. The macOS binaries are not
notarized: if a browser download is blocked, run
`xattr -d com.apple.quarantine mux`.

### From source

It needs Rust 1.91 or newer.

```console
$ cargo install --git https://github.com/yuvichh/mux --locked cli
```

From a checkout, `cargo build --release` produces `target/release/mux`.

### Nix

```console
$ nix run github:yuvichh/mux -- --help
```

## Commands

| Command | Purpose |
| --- | --- |
| `mux cp <SOURCE>... <DESTINATION>` | Copy each source to the destination. |
| `mux sync <SOURCE> <DESTINATION>` | Make the destination hold what the source holds. |
| `mux probe <PLACE>` | Say what is at a place, without copying it. |
| `mux protocols` | List the kinds of place this build can read and write. |
| `mux id` | Print this machine's node id. |
| `mux seed <PATH>...` | `mux cp <PATH>... peer:`, under its first name. |
| `mux get <TICKET> --out <DIR>` | `mux cp <TICKET> <DIR>/`, under its first name. |

A place is written as a path, a web address, a ticket, or `peer:` for this
machine offering to others:

| Place | Written as | Read | Written |
| --- | --- | --- | --- |
| A file or folder | `./data`, `/srv/data`, `file:///srv/data` | yes | yes |
| A web address | `https://example.org/big.iso` | yes | no |
| What other machines hold | `mux://<id>?peer=…` | yes | |
| This machine, offering | `peer:` | | yes |

[docs/commands.md](docs/commands.md) has every flag, what each command
prints, the lines written with `--json`, and the exit codes.

## Coming from p2rent 0.2

- **0.3 does not talk to 0.2.** The way two machines connect and prove who
  they are changed. Move every machine together.
- The command is `mux`, tickets begin `mux://`, and the key variable is
  `MUX_KEY`. A machine that ran p2rent keeps its identity: its key is
  copied to the new configuration folder the first time `mux` runs.
- `seed` and `get` work as before.
- By default a machine now uses public relays and a directory to reach
  machines behind routers. `--no-relay` uses neither, as 0.2 did.

[CHANGELOG.md](CHANGELOG.md) lists everything that changed.

## Security

- A ticket is a capability. Anyone who has it and can reach a machine that
  holds the data can fetch it. Treat a ticket for private data as a secret.
- Machines are identified by key, not by address. Connections are QUIC
  with TLS 1.3, authenticated in both directions against those keys. A
  machine talks only to the ones named in its ticket or introduced by
  those.
- Integrity does not depend on trusting anybody. All data is verified
  against the hash in the ticket.
- A copy from the web is as trustworthy as the server and the connection
  to it, unless it is given `--hash`.
- Links are never followed when offering, and a manifest received from
  another machine is validated so that no path can leave the destination.

## Limitations

mux is not yet production software.

- By default a machine uses the public relays and the directory run by the
  makers of [iroh](https://github.com/n0-computer/iroh). Both learn its
  node id and its network addresses, and neither can read what is
  transferred. `--relay` uses a relay of your own and `--no-relay` uses
  none.
- There are no upload rate limits, and no access control beyond holding
  the ticket.
- A web address can be read and not written. Cloud storage is not a place
  yet.
- A file that has changed is fetched again whole between machines if its
  parts have moved. Chunks that stayed at the same offset are kept.
- If an offered file changes on disk, its chunks stop verifying and other
  machines drop that source. Offer it again to publish the new contents.
- It is developed on macOS and Linux. Windows is covered by the test suite
  in CI and has not been used beyond that.

## Documentation

| Document | Covers |
| --- | --- |
| [Commands](docs/commands.md) | Every command, its flags, what it prints and how it ends. |
| [How it works](docs/how-it-works.md) | An illustrated walk-through of a transfer between machines. |
| [Architecture](docs/architecture.md) | The crates, the data model, storage and the transfer loop. |
| [Protocol](docs/protocol.md) | Tickets, the wire messages and the manifest format. |
| [Performance](docs/performance.md) | How speed is measured, the figures, and what was tried. |
| [Scope](docs/scope.md) | What the project is for, and what it will not do. |
| [Proofs](proofs/README.md) | Which parts are formally proven, and what the proofs rest on. |
| [Changelog](CHANGELOG.md) | What changed for someone who uses `mux`, release by release. |

## Repository layout

| Path | Holds |
| --- | --- |
| `crates/model` | The data model and the logic that is proven: hashes, manifests, chunk layout, the schedule, how a place is named. No I/O, and no standard library. |
| `crates/engine` | What is the same for every place data goes: checking chunks, sharing the work between suppliers, and committing a file when it is whole. |
| `crates/fs` | Local folders: hashing files, and reading and writing them. |
| `crates/web` | Web addresses: reading a file in parts, carrying on after a break, and noticing a file that changes. |
| `crates/peer` | Node identity, connections between machines, the wire protocol and the swarm. |
| `crates/cli` | The `mux` command, and the `mux` library that gathers the others. |
| `benches` | Benchmarks. |
| `fuzz` | Fuzz targets for everything that parses bytes from another machine. |
| `proofs` | Lean 4 proofs of the logic in `crates/model`. |
| `xtask` | Development tasks, written in Rust. |

## Roadmap

[docs/scope.md](docs/scope.md) describes where the project is going, and
the [milestones](../../milestones) track the work. Next: sending only the
changed parts of a file, keeping a folder in sync as it changes, and cloud
storage as a place.

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
