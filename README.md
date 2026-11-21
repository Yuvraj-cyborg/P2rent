<div align="center">
  <img src="assets/logo.png" alt="mux" width="220">
  <p><strong>Copy and sync files between machines.</strong></p>
</div>

mux is a command-line tool for Linux, macOS and Windows. Share a folder with
a ticket, download from the web, or sync a snapshot into an existing directory.
Peers connect directly when possible and can use an encrypted relay connection.

## Install

Download the executable for your machine.

| Platform | Download |
| --- | --- |
| macOS | [Apple silicon][mac-arm] · [Intel][mac-intel] |
| Linux (glibc 2.17+) | [x86-64][linux-intel] · [ARM64][linux-arm] |
| Windows | [x64 executable][windows] |

On **macOS or Linux**, you can also install from a terminal. Run these commands in a writable directory.

```sh
asset="mux-$(uname -s)-$(uname -m)"
curl -fLO "https://github.com/yuvichh/mux/releases/download/v0.3.0/$asset" &&
  mkdir -p "$HOME/.local/bin" && install -m 755 "$asset" "$HOME/.local/bin/mux"
```

If your shell cannot find `mux`, run `export PATH="$HOME/.local/bin:$PATH"`.
On **Windows**, run `.\mux.exe --version` in PowerShell from the download
folder. Put `mux.exe` in a directory on your `PATH` to use it from anywhere.
The [release][release] includes archives and SHA-256 checksums.

## Share a folder

On the sender, run this command and leave it running. Send the full ticket it
prints to the recipient. The ticket holds a secret: whoever has it can
download the snapshot while the command runs, and nobody else can.

```sh
mux cp ./photos peer:
```

To let fewer machines in, add one of these.

| Add | Who can download |
| --- | --- |
| `--once` | Only the first machine that uses the ticket. |
| `--to ID` | Only the machine with that id. Run `mux id` on it to print the id. |
| `--swarm` | Anyone with the ticket, and they also download from each other. |

On the receiver, replace `TICKET` with the ticket you received. This creates
`./downloads/photos`. Run the same command again to resume an interrupted transfer.

```sh
mux cp 'TICKET' ./downloads/
```

## Sync an existing folder

Use `sync` to update the exact directory you name. Preview the changes first.

```sh
mux sync 'TICKET' ./backup/photos --dry-run
mux sync 'TICKET' ./backup/photos
```

A ticket identifies one snapshot. To pull later edits, offer the updated
folder and sync its new ticket to the same destination. Mux verifies and
reuses local chunks, including chunks shifted by an insertion. Extra local
files stay in place unless you pass `--delete`. Replaced and deleted files
go to `.mux-trash` by default.

## More commands

| Command | Use |
| --- | --- |
| `mux cp URL ./downloads/` | Download a file over HTTP or HTTPS. |
| `mux cp ./photos ./archive/` | Copy a local directory with its metadata. |
| `mux probe 'TICKET'` | Inspect a snapshot and see which peers have it. |

For scripting, `--json` writes one JSON object per line. See the [command
reference](docs/commands.md) for exit codes, checksum verification, mirrors
and relay options.

**Upgrading from p2rent 0.2?** Install mux on every participating machine
and generate new tickets. Mux keeps the existing device identity on first
use. See the [migration notes][release].

[How it works](docs/how-it-works.md) · [Performance](docs/performance.md) ·
[Architecture](docs/architecture.md) · [Changelog](CHANGELOG.md) ·
[Contributing](CONTRIBUTING.md) · [MIT license](LICENSE)

[release]: https://github.com/yuvichh/mux/releases/tag/v0.3.0
[mac-arm]: https://github.com/yuvichh/mux/releases/download/v0.3.0/mux-Darwin-arm64
[mac-intel]: https://github.com/yuvichh/mux/releases/download/v0.3.0/mux-Darwin-x86_64
[linux-arm]: https://github.com/yuvichh/mux/releases/download/v0.3.0/mux-Linux-aarch64
[linux-intel]: https://github.com/yuvichh/mux/releases/download/v0.3.0/mux-Linux-x86_64
[windows]: https://github.com/yuvichh/mux/releases/download/v0.3.0/mux.exe
