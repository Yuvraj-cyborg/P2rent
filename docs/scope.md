# Scope

mux moves and syncs data between any two places: a folder, a web URL,
another machine and, later, cloud storage and torrents. It is one
command-line tool and one library, and it gives the same guarantees wherever
the data comes from and wherever it goes.

The model is FFmpeg. Media had hundreds of incompatible formats, and FFmpeg
became the one tool that reads any of them and writes any of them, built on a
library that other programs embed. Moving data is in that state today: every
place has its own tool, each with its own rules for resuming, checking and
speed.

This document describes where the project is going. The [README](../README.md)
describes what the released version does.

## Commands

| Command | Purpose |
| --- | --- |
| `mux cp SRC... DST` | Move data once. |
| `mux sync SRC DST` | Make `DST` match `SRC`. `--watch` keeps it matching. |
| `mux probe WHERE` | Report what is at a place: size, files, hashes, who holds it. |
| `mux protocols` | List the kinds of place this build can read and write. |

A place is a path or a URL. The commands work out everything else from the two
places they are given, and there is no configuration file.

| Place | Written as | Milestone |
| --- | --- | --- |
| Local folder or file | `./data`, `/srv/data` | 1 |
| Web URL | `https://example.org/big.iso` | 1 |
| Another machine | `peer:` to serve, a `mux://` ticket to fetch | 1 |
| Cloud storage | `s3://`, `gs://`, `az://`, `sftp://` | 3 |
| Model and container registries | `hf://`, `oci://` | 3 |
| Torrents | `magnet:`, `.torrent` files | 3 |
| A pipe | `-` | 3 |

## Guarantees

These hold for every place, which is the reason to use one tool.

1. **Every byte is checked.** A transfer is reported as *verified* when its
   hash was known before it started, and as *consistent* when no hash was
   available, the size and the server's validator stayed stable, and the hash
   was computed and reported. The tool never claims more than it checked.
2. **No half-written files.** A file appears under its final name only when
   it is complete and verified.
3. **Transfers resume.** Running the same command again fetches only what is
   missing.
4. **Several sources feed one transfer.** A URL and any number of machines
   can supply the same download, and every machine that has part of the data
   can serve it.
5. **Only what changed is sent.**
6. **Sync does not destroy data.** Deleted and overwritten files go to a
   trash folder first, a sync that would delete a large share of a folder
   stops and asks, and a missing record is never treated as a deletion.

Guarantees 1, 3 and 4 hold today between machines. The rest arrive with the
milestones below.

## What it is not

- **Not a service.** There are no accounts and no server that can read the
  data. Relays, where they are used, forward encrypted traffic.
- **Not a backup tool.** It keeps no history of old versions beyond the
  trash folder.
- **Not a network file system.** Nothing is mounted.
- **Not a web client.** No cookies, logins or crawling.
- **Not an anonymity tool.** Machines that transfer to each other see each
  other's addresses.
- **Not an application with a window.** The library is built so that others
  can make one.

## Layout

Crates are named for what they hold, with no project prefix. `model`,
`engine`, `fs`, `web`, `peer` and `cli` exist today. Later milestones add
the rest.

| Crate | Holds |
| --- | --- |
| `model` | The data model, the `Source` and `Sink` traits and the logic of a transfer. No disk and no network, so its logic can be proven. |
| `engine` | Carries that logic out: the tasks, locks and timers a transfer needs, which `model` must not have. |
| `fs` | Local folders. |
| `web` | Web URLs. |
| `peer` | Machine to machine. |
| `cloud` | Cloud storage, through Apache OpenDAL. |
| `sync` | The sync engine. |
| `accel` | The only crate allowed `unsafe`, SIMD and assembly. |
| `cli` | The `mux` command, and the library other programs depend on. |
| `ffi` | A C API, then bindings for other languages. |

Shared logic has one home, `model`. A backend is a thin adapter: it
does no scheduling, retrying or verifying of its own.

The names the crates are published under are decided when publishing
happens ([#25](https://github.com/yuvichh/mux/issues/25)): `mux` is
taken on crates.io, and a name as plain as `fs` cannot be published either.

## Milestones

| | Goal | Release |
| --- | --- | --- |
| [0](https://github.com/yuvichh/mux/milestone/1) | Foundations: analysis, verification, proofs, performance baseline, workspace. No visible change. | |
| [1](https://github.com/yuvichh/mux/milestone/2) | `cp` between a folder, a URL and another machine. | 0.3 |
| [2](https://github.com/yuvichh/mux/milestone/3) | One-way `sync`. | 0.4 |
| [3](https://github.com/yuvichh/mux/milestone/4) | More places. | 0.5 |
| [4](https://github.com/yuvichh/mux/milestone/5) | Pluggable anywhere: a stable library, a C API, bindings. | 0.6 |
| [5](https://github.com/yuvichh/mux/milestone/6) | Two-way `sync`. | towards 1.0 |

The rules every change follows are in [CONTRIBUTING.md](../CONTRIBUTING.md).
