# Commands

This describes the commands on the `main` branch. Release 0.2.0 has `seed`,
`get` and `id` only, under the name `p2rent`.

Progress and notes for people go to standard error. What a command produced
goes to standard output, so it can be piped.

## `mux cp`

```console
$ mux cp <SOURCE>... <DESTINATION>
```

Copies each source to the destination. A source is a web address, a file, a
folder, or a ticket for what another machine offers. The destination is a
folder, or `peer:`, which offers the sources to other machines.

```console
$ mux cp https://example.org/big.iso ./downloads/      # ./downloads/big.iso
$ mux cp https://example.org/big.iso ./image.iso       # under a name of its own
$ mux cp ./weights ./notes.txt /mnt/backup/            # a folder and a file
$ mux cp ./weights peer:                               # offer it, and print a ticket
$ mux cp 'mux://450b9c6b…' ./models/                   # ./models/weights, from whoever has it
```

The destination is read as `cp` reads it. If it is a folder, or is written
with a slash at its end, the copy goes inside it under the name the source
has. Otherwise it is the name the copy is given.

| Option | Meaning |
| --- | --- |
| `--hash <DIGEST>` | For a source on the web. The digest the source must have: `blake3:` or `sha256:` and 64 hex characters. A copy that does not have it is not kept. |
| `--mirror <URL>` | For a source on the web. Another address that holds the same file, read together with the source. Repeatable. It needs a digest of the file, from `--hash` or from a server that sends one: nothing else tells that two servers hold the same bytes. |
| `--streams <N>` | The most requests to make at once to each source, each for a part of a file. The default is 8. A server that refuses some of them is used with those it takes. With 1, a file is one request. |
| `--stall-timeout <SECONDS>` | Give up after this long with next to nothing received: under about a kilobyte a second on every stream. A slow link is waited for. The default is 60. |
| `--dry-run` | Say what would be copied, in the same form as a result, and copy nothing. A source on the web is asked what it holds and for none of the file. |
| `--json` | Write for a program: see [below](#for-a-program). |

A flag may be written before, between or after the places.

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

### To and from another machine

```console
$ mux cp ./weights peer:
seeding  weights  (400.68 MiB, 402 chunks)
mux://450b9c6b…6d2bfc54?peer=35964d95…f14dd0e7
```

`mux cp <PATH>... peer:` offers files or folders on this machine to others.
It writes one ticket for each path to standard output, and goes on serving
until it is stopped with Ctrl-C. Only what is on this machine can be
offered.

```console
$ mux cp 'mux://450b9c6b…6d2bfc54?peer=35964d95…f14dd0e7' ./models/
./models/weights	420143104	verified
```

`mux cp <TICKET> <FOLDER>` fetches what the ticket names, from every machine
that holds it.

- Every chunk is compared with the digest the ticket carries, so the copy
  is always `verified`. `--hash` and `--mirror` are not for a ticket.
- What a ticket names keeps its name. The destination is the folder to put
  it in, written with a slash at its end if it is not there yet.
- A copy that was interrupted is carried on with when the command is run
  again: what is there is checked, and only what is missing is fetched.
- `--streams` is not used. A machine is asked for as many chunks at once as
  its connection has room for.

A ticket is all another machine needs, and whoever has it can fetch the
data. These flags say how the two machines reach each other. They are the
same for `cp`, `probe`, `seed` and `get`:

| Flag | |
| --- | --- |
| `--listen <ADDR>` | The address to listen on. By default every address, at a port the system picks. |
| `--relay <URL>` | Reach machines behind routers through this relay, in place of the public ones. |
| `--no-relay` | Use no relay and no directory. A machine is then reached at an address or not at all, and a ticket carries this machine's own. |
| `--congestion <cubic\|bbr>` | How a connection decides how fast to send. `cubic`, the default, halves its rate at each lost packet. `bbr` holds the rate the link carries whatever is lost: many times faster over a link that loses one packet in a hundred, and somewhat slower than `cubic` on a link that loses none. It is the sender's choice, so give it to the machine that offers. |
| `--key <FILE>` | This machine's key, which is its name to other machines. Also `MUX_KEY`. It is made the first time a command has to do with another machine. |

## `mux seed` and `mux get`

`mux seed <PATH>...` is `mux cp <PATH>... peer:`, and `mux get <TICKET>
--out <FOLDER>` is `mux cp <TICKET> <FOLDER>/`. They came first, and have a
few flags of their own:

| Flag | |
| --- | --- |
| `seed --advertise <HOST[:PORT]>` | An address to put in the ticket for other machines to dial. Repeatable. |
| `seed --chunk-size <SIZE>` | 4K to 16M. The default is 1M. |
| `get --seed` | Keep serving to other machines after the download. |
| `get --peer <NODE[@HOST:PORT]>` | Another machine to try, as well as those in the ticket. Repeatable. |
| `get --max-peers <N>` | The most machines to fetch from at once. The default is 8. |

## `mux probe`

```console
$ mux probe <PLACE>
```

Says what is at a web address, a file, a folder or a ticket, and reads none
of it. A web server is asked for the first byte of the file. The machines a
ticket names are asked what it holds, and how much of it each of them has.

```console
$ mux probe https://example.org/big.iso
name     big.iso
kind     file
size     1288490188 bytes (1.20 GiB)
files    1
checked  consistent, if it is copied
parts    read several at once
```

`checked` is how far a copy of the place would be checked. It is `verified`
if a digest of every file is known: one given with `--hash`, or one a web
server sends, which is then listed. `parts` says whether the place can be
read at several points at once, or only in order from its start.

```console
$ mux probe 'mux://450b9c6b…6d2bfc54?peer=35964d95…f14dd0e7'
name     weights
kind     folder
size     420143104 bytes (400.68 MiB)
files    12
checked  verified, if it is copied
parts    read several at once
holder   35964d95  all of it
```

For a ticket there is a `holder` line for each machine the ticket names:
`all of it`, how many bytes of it, or `could not be asked` for a machine
that did not answer.

## `mux sync`

```console
$ mux sync <SOURCE> <DESTINATION>
```

Makes the destination hold what the source holds, and reads from the source
only what the destination lacks or has in another version. The source is a
web address, a file or a folder. The destination is the folder, or the
file, that is made to match: `mux sync ./a ./b` makes `./b` match `./a`,
where `mux cp ./a ./b/` puts a copy of `a` inside `b`.

```console
$ mux sync ./weights /mnt/backup/weights --delete
remove	old-notes.txt
add	run-7/config.json
replace	run-7/model.bin
```

Standard output has one line for each path where there was something to do,
with a tab between the step and the path. `.` is the destination itself.

| Step | Meaning |
| --- | --- |
| `add` | The destination had nothing there. |
| `replace` | The destination had something else there. |
| `touch` | It held the same, with other permissions or another time. |
| `remove` | The source has nothing there. Only with `--delete`. |
| `blocked` | The destination has a folder where the source has a file or a link. Without `--delete` nothing is removed, so it is left as it is, and the command ends with 1. |

| Option | Meaning |
| --- | --- |
| `--dry-run` | Say what would be done, and do none of it. |
| `--delete` | Take away what the destination has and the source has not. Without it nothing is ever removed. |
| `--force` | Go on though more than half of what the destination has would be removed. |
| `--no-trash` | Keep nothing of what is replaced or removed. |
| `--streams`, `--stall-timeout` | As for `cp`. |
| `--json` | Write for a program: a `step` line for each step, the lines of the copy, and a `synced` line at the end, or `sync_plan` with `--dry-run`. |

### What keeps a sync from losing files

- **Nothing is removed unless it is asked for.** Without `--delete`, what
  only the destination has is left alone.
- **What is replaced or removed goes to a trash folder first:**
  `.mux-trash` inside the destination, with a folder for each sync, and
  each thing under the path it had. A file that is being replaced stays
  where it is until the new one is whole. The trash is never synced or
  removed by `mux`. Delete it when it is not wanted, or pass `--no-trash`.
- **A sync that would remove more than half of what the destination has
  stops** before it changes anything, and says so. That is what a sync
  from a folder that is empty by mistake looks like. `--force` goes on.
- **A file has its final name only when it is whole,** as with `cp`.
- **A sync that is interrupted is run again.** It looks at the two places
  as they are and does what is left.

### What counts as the same

Two files are taken to hold the same if they have the same size and were
last changed in the same second. This is the check `rsync` makes by
default. Parts of a second are not compared, because file systems keep them
to different lengths. A source that gives no time for a file, as some web
servers do, is replaced every time.

What a system cannot keep is not compared. On Windows that is a file's
permissions beyond whether it may be written, and a folder's permissions
and time.

A file is synced with a file and a folder with a folder. The decision for
each path is proven: see [proofs](../proofs/README.md).

## For a program

With `--json`, a command writes one JSON object on each line of standard
output and nothing else there. Every line has `v`, the version of the shape
of the lines, which is 1, and `event`. A later version of mux may add
fields to a line without changing `v`. It will change `v` before it takes a
field away or changes what one means.

| `event` | When | Fields |
| --- | --- | --- |
| `start` | A copy begins. | `source` and `destination` as they were typed, `bytes` and `files` to copy |
| `progress` | From time to time, and when all of it is there. | `bytes` at the destination, `of` how many |
| `file` | A file is whole and has just been given its final name. Each file that is copied gets one line: a file with no bytes as the copy begins. A file that was there already and is kept gets none. | `path` |
| `done` | A copy is finished. | `path`, `bytes`, `checked`, `fetched`, and `kept` from an earlier copy |
| `plan` | In place of all the above, with `--dry-run`. | `source`, `destination`, `path`, `bytes`, `files`, `checked` |
| `probe` | The answer of `mux probe`. | `place`, `name`, `kind`, `bytes`, `files`, `checked`, `parts`, and `digests`, a list of `path` and `digest`. For a ticket also `holders`, a list of `node` and `bytes`: how much of it that machine has, or `null` if it could not be asked |
| `offer` | A path is offered to other machines, by `cp` to `peer:` or by `seed`. | `ticket`, `name`, `bytes`, `files` |
| `step` | One thing a sync does, or would do with `--dry-run`. | `step` and `path`, as on standard output without `--json` |
| `synced` | A sync is finished. | `source`, `destination`, how many paths got each step (`add`, `replace`, `touch`, `remove`, `keep`, `blocked`), `bytes` brought, and `trash`: where what was replaced or removed is, or `null` |
| `sync_plan` | In place of `synced`, with `--dry-run`. | The same, without `trash` |
| `error` | The command failed. It is the last line. | `code`, the exit code; `kind`, the same in a word; `message`; `hint`, or `null` |

`checked` is `verified`, `consistent` or `already there`, as on standard
output without `--json`. The words for `kind` are `failed`, `usage`,
`absent`, `unreachable`, `changed` and `corrupt`, in the order of the exit
codes from 1.

```console
$ mux cp --json https://example.org/a.bin ./out/
{"v":1,"event":"start","source":"https://example.org/a.bin","destination":"./out/","bytes":1100000,"files":1}
{"v":1,"event":"progress","bytes":1100000,"of":1100000}
{"v":1,"event":"file","path":"./out/a.bin"}
{"v":1,"event":"done","path":"./out/a.bin","bytes":1100000,"checked":"consistent","fetched":1100000,"kept":0}
```

A person's error message is still written to standard error.

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

2 is for a command that asks for what `mux` does not do, however often it
is run: too few places, a place that is not one, two places that cannot be
copied between, a flag that is not for that kind of place, a folder copied
into itself, a new name for what a ticket names. The command has to be
written another way. A command refused like this copies nothing, opens no
socket and makes no key.

A sync that stops because of what it found ends with 1: a folder in the
way, more than half of the destination to remove, a file where a folder
is. The message says which flag goes on, if that is what is meant.
