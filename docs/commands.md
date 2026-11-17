# Commands

This describes the commands on the `main` branch. Release 0.2.0 has `seed`,
`get` and `id` only, under the name `p2rent`.

Progress and notes for people go to standard error. What a command produced
goes to standard output, so it can be piped.

## `mux cp`

```console
$ mux cp <SOURCE>... <DESTINATION>
```

Copies each source to the destination. Today a source is a web address, a
file or a folder, and the destination is a folder. Copying to and from
another machine will follow, and until then `mux seed` and `mux get` do
that.

```console
$ mux cp https://example.org/big.iso ./downloads/      # ./downloads/big.iso
$ mux cp https://example.org/big.iso ./image.iso       # under a name of its own
$ mux cp ./weights ./notes.txt /mnt/backup/            # a folder and a file
```

The destination is read as `cp` reads it. If it is a folder, or is written
with a slash at its end, the copy goes inside it under the name the source
has. Otherwise it is the name the copy is given.

| Option | Meaning |
| --- | --- |
| `--hash <DIGEST>` | For a source on the web. The digest the source must have: `blake3:` or `sha256:` and 64 hex characters. A copy that does not have it is not kept. |
| `--mirror <URL>` | For a source on the web. Another address that holds the same file, read together with the source. Repeatable. It needs a digest of the file, from `--hash` or from a server that sends one: nothing else tells that two servers hold the same bytes. |
| `--streams <N>` | Parts of a file to fetch at once from each source. The default is 4. |
| `--stall-timeout <SECONDS>` | Give up after this long with nothing received. The default is 60. |

### What a copy does

- It is written under a name ending `.mux-part` and renamed when it is
  whole, so a file with its final name is always a whole one.
- A copy that was interrupted is carried on with when the command is run
  again, if the server tells versions of the file apart (an `ETag` or a
  `Last-Modified`) and sends parts of it on request.
- If the file changes on the server while it is read, the copy ends with an
  error and nothing of it is kept.
- A file that is already there with the size and time the server gives is
  left alone and not read.

A folder is copied with everything in it: its files, the folders that hold
nothing, links that stay inside it, permissions and the time each file was
last changed. Special files, links that lead out of the folder, and copies
that were never finished are left out, and a note says how many. Every file
is written under a part name first, as above.

Copying a folder again copies all of it again, unless every file is already
there with its size and time. Copying only what has changed is what `sync`
will do. A folder cannot be copied into itself.

### Standard output

One line for each source, with tabs between its three parts: the path that
was written, its size in bytes, and how far it was checked.

| Checked | Meaning |
| --- | --- |
| `verified` | Every byte was compared with a digest given for the file. |
| `consistent` | No digest was known. The source stayed what it was throughout. For a folder, nothing checks that: its files are read once, as they are. |
| `already there` | Every file was there with the size and time the source gives, and nothing was read. |

## Exit codes

They are the same for every command and do not change between versions.

| Code | Meaning | Worth running again |
| ---: | --- | --- |
| 0 | Done. | |
| 1 | It failed for a reason not listed below. | |
| 2 | The command line is wrong, or names a place that cannot be used. | No. |
| 3 | Nothing is at the source. | No. |
| 4 | The source could not be reached, or stopped sending. | Yes: it carries on with what it stored. |
| 5 | The source changed while it was being read. | Yes: it copies the source as it is now. |
| 6 | What arrived is not what was asked for: it does not have the digest given. | No. |

An error is one line on standard error beginning `error:`. Where there is
something to do about it, a line beginning `hint:` follows.
