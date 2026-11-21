# Changelog

What changed for someone who uses `mux`, release by release. The newest is
first. Each line names the pull request that made the change.

## Unreleased

### Coming from 0.3.0

- **This version does not talk to 0.3.** A request now has to prove that
  it knows the secret of a ticket, and a 0.3 ticket has none. Every machine
  has to move together, and tickets have to be made again. An old ticket is
  refused with a line that says so (#126).
- **A machine that fetches no longer serves.** In 0.3 every machine that
  copied from a ticket also gave what it had to anyone who named the
  content, and was listed to the others. Now that happens only for a ticket
  made with `--swarm`. A fleet that fills from one machine adds `--swarm`
  where it offers (#126).
- `--seed` after a ticket that is not a swarm's writes a ticket of its own
  for the copy (#126).

### Added

- **A secret in every ticket.** A machine that offers answers only who
  proves it knows the secret. Without it a machine learns nothing, not even
  whether the files are there. Each offer has a new secret, so stopping an
  offer makes its ticket useless for good (#126).
- **`--to NODE`** lets only the machines that are named fetch, and
  **`--once`** only the first machine that asks. A machine that is not let
  in ends with the new exit code 7, `refused` (#126).
- **`--swarm`** makes a ticket whose holders fetch from each other, as
  every ticket did before (#126).
- `mux probe` says whether a ticket is a swarm's (#126).

- **Install in one line** on macOS and Linux:
  `curl -fsSL https://raw.githubusercontent.com/yuvichh/mux/main/install.sh | sh`.
  It takes the build for the machine, checks it against the release's
  SHA-256 digest, and puts it in `~/.local/bin` (#127).
- **Homebrew**: `brew install yuvichh/tap/mux` (#127).

### Changed

- An error message names a ticket by the id of what it offers, and leaves
  its secret out (#126).

### Fixed

- The Nix flake builds again: `nix profile install github:yuvichh/mux`. It
  had left out a member of the workspace (#127).

## 0.3.0

[Release notes and downloads](https://github.com/yuvichh/mux/releases/tag/v0.3.0)

The project was called p2rent up to 0.2.0. It is now **mux**, and it has
grown from one use (hand a folder to many machines) into a tool that
copies and syncs between folders, web addresses and other machines.

### Coming from 0.2.0

- **0.3 does not talk to 0.2.** The way two machines connect and prove
  who they are changed (#68). Every machine has to move to 0.3 together.
- The command is `mux`, tickets begin `mux://`, and the key variable is
  `MUX_KEY`. A machine that ran p2rent keeps its identity: its key is
  copied to the new configuration folder the first time `mux` runs (#36).
- `seed` and `get` still work as before. They are now two spellings of
  `mux cp` (#99, #107).
- Building from source needs Rust 1.91 or newer.

### Added

- **`mux cp`** copies each source to a destination (#69, #70, #99, #108).

  | From | To | |
  | --- | --- | --- |
  | a web address | a folder | several connections, carried on after a break |
  | a file or folder | a folder | with links, permissions and times |
  | a file or folder | `peer:` | offers it to other machines and prints a ticket |
  | a ticket | a folder | fetches from every machine that has it |

- **The web and other machines in one transfer.** `mux cp URL DIR --seed`
  copies from the web and goes on offering the file. Its ticket names the
  web address too, and a machine with that ticket reads from both, checking
  every chunk against the first machine's copy (#105, #108).
- **`mux sync`** makes a folder match a source and reads only what has
  changed. Nothing is removed without `--delete`. What is replaced or
  removed goes to a trash folder first. A sync that would remove more than
  half of a folder stops (#77, #78).
  `mux sync TICKET DESTINATION` pulls a peer snapshot into the named file
  or folder, reusing verified local chunks and resuming interrupted syncs (#117).
- **`mux probe`** says what is at a place without copying it: size, files,
  how far a copy would be checked, and for a ticket, which machine holds
  how much (#71, #99).
- **`--json`** on every command: one JSON object on each line, with a
  version number (#71).
- **`--dry-run`** says what a copy or a sync would do (#71, #77).
- **Exit codes** that say whether running the command again is worth it
  (#69, #100).
- **`--hash`** verifies a copy from the web against a digest, and
  **`--mirror`** reads the same file from a second server at once (#67,
  #69).
- **Machines behind routers** are reached: directly where their routers
  allow it, through a relay where they do not. `--relay` uses a relay of
  your own and `--no-relay` uses none (#68).
- **`--congestion bbr`** for links that lose packets for reasons other than
  being full. `cubic` stays the default (#92).
- **`mux protocols`** lists the kinds of place a build can use (#52).

### Changed

- Peer downloads now recover verified chunks even when an insertion shifts
  their offsets. The sync summary reports the bytes fetched and reused
  (#114, #116, #117).

- **Only whole files appear.** A file is written under a name ending
  `.mux-part` and renamed when every chunk of it is in place (#40).
- **The whole tree arrives:** empty folders, links that stay inside the
  tree, permissions and times. 0.2.0 kept only the execute bit (#42).
- **The same bytes are one swarm,** whatever two machines call their
  copies (#42).
- **A copy from the web is as fast as curl or faster.** Level with it
  where one connection fills the link, 4 to 9% faster far from the
  server, and several times faster where packets are lost or the link is
  fast and far (#86, #90, #95).
- **Between machines, a transfer asks for as many chunks at once as the
  route has room for,** in place of a fixed number (#81).
- **A slow link is no longer taken for a dead one.** Progress is counted
  in bytes, not in whole chunks (#85).
- **A transfer that has nobody left to fetch from says so at once,** and
  does not wait for a timeout (#64).
- **A transfer does not wait for its slowest supplier.** With a slow web
  server beside a quick machine, the quick one does the work: 9.3 s became
  1.2 s in the case measured (#111).

### Fixed

- Indexing took a file's size and time from the listing of its folder,
  which can be out of date. It now asks the file itself (#45).
- `seed` said nothing of what it left out (#47).
- A bitfield with stray bits past its end could make a transfer count
  itself complete with a chunk missing. Found by checking the proofs, and
  now proven not to happen (#94).
- A file with no bytes was never reported to a program reading `--json`
  (#101).

### Checked

- The rules that decide what is fetched, where a path may point and what a
  sync does to each path are proven in Lean, and the statements of those
  proofs were themselves audited by putting 348 deliberate bugs into the
  code: 345 are noticed, and the other three change nothing (#34, #37,
  #43, #77, #87, #94, #106).
- Every place data can be read from or written to passes one set of rules,
  and every transfer is tested against suppliers that lie, stall, break
  off and change their minds (#54, #55).
- `mux` is killed at random moments in its tests. Whenever it stops, every
  file with its final name is whole (#40, #96).

## 0.2.0

The rewrite: `seed` and `get` with a ticket, every downloader also a
source, chunks verified against a BLAKE3 manifest, QUIC with keys on both
ends. Released as `p2rent`.
[Release notes](https://github.com/yuvichh/mux/releases/tag/v0.2.0).
