# Architecture

mux is built from five crates. Each depends only on the ones above it in
this table.

| Crate | Responsibility | Knows about |
| --- | --- | --- |
| `model` | The data model and the logic that is proven: hashes, manifests, chunk layout, bitfields, the schedule of which chunk to fetch next, and how a place is named: locations and tickets | nothing else |
| `engine` | What is the same for every place and every supplier: fetching chunks from many suppliers at once, checking a chunk before it is stored, and having a file committed when its last chunk is in | model |
| `fs` | Local folders: hashing files into artifacts; reading and writing byte ranges; part files that are renamed into place when committed | model |
| `peer` | Node identity, authenticated QUIC, wire messages, and the swarm: what a node serves, how it answers peers, and a peer as a supplier of chunks | model, engine, fs |
| `cli` | The `mux` command (argument parsing, progress bars, output) and the `mux` library, which re-exports the others | all of the above |

`model` touches no disk, network or clock and does not use `std`.
`just pure` builds it for a target that has no standard library and checks
that it depends only on an allowed list of crates. That is what keeps its
logic small enough to prove.

```
crates/
├── model/src/
│   ├── artifact/      hash.rs  digest.rs  manifest.rs  path.rs  layout.rs  bitfield.rs  wire.rs
│   ├── assembly.rs
│   ├── schedule.rs
│   ├── location.rs
│   ├── mirror/        mod.rs  step.rs
│   ├── notes.rs
│   ├── place.rs
│   ├── retry.rs
│   ├── ticket.rs
│   └── hex.rs
├── engine/src/        store.rs  transfer.rs  source.rs  copy.rs  claims.rs  conformance.rs  faults.rs
├── fs/
│   ├── src/           index.rs  folder.rs  io.rs
│   └── tests/folder/  what a store over a folder leaves on disk
├── web/
│   ├── src/           headers.rs  origin.rs
│   └── tests/origin/  a web server that misbehaves, and what is made of it
├── peer/
│   ├── src/
│   │   ├── net/       identity.rs  tls.rs  endpoint.rs  proto.rs
│   │   ├── swarm/     node.rs  share.rs  server.rs
│   │   │   └── download/  mod.rs  supplier.rs
│   └── tests/         connection.rs  download.rs  swarm.rs  properties.rs
└── cli/
    ├── src/           lib.rs  main.rs  cli/
    └── tests/         cli.rs
benches/               instructions.rs  transfer.rs  web.rs  delta.rs (see docs/performance.md)
fuzz/                  fuzz targets, a workspace of their own
proofs/                Lean proofs of the logic in `model`
xtask/                 development tasks written in Rust (`cargo xtask`)
```

## Data model

An **artifact** is a file or a directory tree. Its **manifest** has two
halves. The **content** lists every entry (files with their sizes,
directories, and links with their targets) and the BLAKE3 digest of every
chunk. The **metadata** holds the root's name and, for each entry, its
permission bits and the time it was last modified.

In code, the entries, their metadata and the way the files are cut into
chunks are a `Tree`. An `Artifact` is a tree together with the digest of
each chunk and the identifiers that follow from them. A place such as a
folder holds a tree: it is told which digests to check what it already has
against, and knows nothing else about them.

There are two identifiers. The **content id** is a hash of the content
alone, and names the swarm: any node holding those bytes can supply them,
whatever its copy is called. The **artifact id** is a hash of the content id
and the metadata, and is what a ticket carries, so a ticket pins both. The
encoding begins with a format version, and a manifest in a version a build
does not know is refused by name. [protocol.md](protocol.md) has the format.

Files are chunked independently at a fixed size, so chunk *n* of a file always
starts at byte *n × chunk size*. Two consequences follow:

- a seeder can serve any chunk with one positional read from the original
  file, with no separate chunk store;
- a downloader can write chunks in any order into pre-sized files.

Manifests come from untrusted peers. `Manifest::validate` runs before a
manifest is used for anything and rejects names that are empty, `.`, `..`,
longer than 246 bytes, that end in `.mux-part`, or that contain a separator,
drive colon or NUL; paths that are out of order or repeated; an entry whose
directory is not itself listed as a directory, which rules out anything
under a file or behind a link; a link whose target climbs above the root;
permission bits beyond read, write and execute; and chunk counts that
disagree with the declared file sizes. The rules on bytes live in
`artifact::path` and are proven: a path that passes stays inside the root,
a link target that passes never climbs above it, and paths in order are all
different.

## Locations

A **location** is the one written form for a place: a path, a web address,
a ticket, or `peer:` for this machine serving to others.
`model::location::Location::parse` is the only reader of that form. It
decides what kind of place a text names and nothing more, so it needs no
disk and no network.

| Written as | Becomes |
| --- | --- |
| `./data`, `/srv/data`, `C:\data` | a local path, kept as typed |
| `file:///srv/a%20b` | the local path it holds, `/srv/a b` |
| `https://example.org/big.iso` | a web address, kept as typed |
| `mux://<id>?peer=…` | a ticket |
| `peer:` | this machine, serving |

A text is a path unless it starts with a scheme of two or more characters
and a colon, so a Windows drive letter is never mistaken for one. A file
whose name would read as something else is written with `./` in front.

Each backend crate declares the **protocols** it provides: the scheme, and
whether such a place can be read, written or both. A build's list is that of
the backends compiled into it, and `mux protocols` prints it. The parser is
handed that list. A location of any other kind is refused with a sentence
that says what the build can use, and whether the scheme is one mux has no
meaning for or one it has reserved for a kind of place it cannot use yet.

Printing a location gives text that reads back as the same place. That and
"reading never panics" are checked on generated input and by a fuzz target.

## Places

A **place** is somewhere data is read from or written to. `model::place` has
the traits a backend implements, and nothing else about a place is known
to the rest of the program.

| Trait | Does |
| --- | --- |
| `Source` | Says what it holds; opens a file to be read in order from an offset; reads a range of bytes. |
| `Sink` | Makes the place ready and says which chunks it already holds; writes bytes at an offset and reads them back; keeps notes for the engine beside a file being written; commits a file; aborts a file; finishes the tree. |
| `Supplier` | Is reached, says which chunks it holds and which other suppliers it knows of, and hands over a chunk. |

A file being written is kept apart from the file under its final name until
it is committed. A failure says which of five things it is (the place is
unreachable, it does not hold what was asked for, it answered with something
it must not, what it holds has changed since the transfer began, or trying
again gains nothing), so the engine never has to know what a socket or a
disk is.

Reading returns a future: bytes are fetched from many sources at once, and a
fetch that is no longer wanted is dropped. Writing is a plain call that does
its work before it returns. The engine makes each one on a thread that is
allowed to wait, and a write that has begun is always finished.

A backend is a thin adapter: it moves bytes. What is the same for every
place is done once, by `engine::Store`:

- a chunk is hashed and compared with the manifest before the place is given
  it;
- the store knows which chunks are held, and reads one back only if it is;
- when a chunk makes a file whole, the file is committed. That decision is
  made by `model::assembly`, which is proven: a file is announced only when
  every chunk of it is in place;
- checking, writing and committing one chunk run as one step on one thread,
  to the end, even if whoever asked for it gives up. So a file is never
  counted as whole and left uncommitted.

`engine::conformance` holds the checks every place must pass, and a place
held in memory that passes them. Each backend's tests run the checks on its
own place.

### When the digests are not known

A machine that seeds knows the digest of every chunk. A web server does not.
`Store::learn` fills a place from a source like that:

- A chunk is taken if it is the right length, and its digest is recorded as
  it is stored. When the last chunk is in, the tree and what was learned are
  an artifact like any other.
- If a digest was given for a whole file, by the user or by the server, the
  file is read back and compared with it before it is committed. A file
  that does not have it is thrown away, and the transfer ends.
- To carry on after an interruption, each digest is also **noted** in the
  place, beside the file being written. The notes begin with what the source
  gives to tell one version of the file from another. Run again, the
  transfer believes the notes only if they are about the same version, and
  a noted chunk only if its bytes still have the digest noted for it.
  `model::notes` is the format, and reading it is fuzzed.
- If a supplier finds that what it is reading has changed, the transfer
  ends and everything not yet committed is thrown away: bytes of two
  versions cannot be told apart.

A finished transfer says how far its bytes were checked. It is *verified*
if every chunk was compared with a digest known beforehand, or every file
with a digest given for it. It is *consistent* if no digest was known and
the source stayed what it was throughout. It is never reported as more than
was checked.

## Storage

`store::index` walks a path, hashes chunks in parallel and returns an
artifact. `Folder` is the place for a local folder: it reads and writes at
an offset, so many chunks can be in flight without sharing a file cursor.

A file under its final name is always whole and correct. While a file is
being downloaded its bytes go into a part file beside it, `<name>.mux-part`.
When the store commits the file, the folder flushes the part file and
renames it over the final name. The directories are flushed once, when the
download completes, so that the renames survive a power cut. So however the
program stops, there is never a file with holes under a name a user would
open, and a file that was already there is replaced in one step or left
alone, never changed in place.

Bytes are read from part files as well as whole ones, so a node serves what
it holds while it downloads the rest.

The engine's notes for a file are kept at the end of its part file, after
the file's own bytes, and are cut off when the file is put in place. So
there is still one file to clean up, and no state file.

Directories are created first, and each must be a real directory: a link
standing where one belongs is an error, so nothing is ever written through a
link to somewhere else. Links are created by the same rename as files. A
file gets its recorded time and permissions just before its rename. A
directory gets them when the download completes, the deepest first, because
filling a directory changes its time and a read-only one cannot be filled.

Resuming needs no state file. When a folder is made ready it re-hashes
whatever is at the destination and reports the chunks that are right. A
whole file is kept. A part file keeps every chunk that is right. A file
under its final name that is not whole, because it was damaged or is another
version, is left as it is until its replacement is ready, and the chunks of
it that are right are copied into the part file instead of being fetched
again. Indexing leaves part files out.

## Web

`web::Origin` is a file at a web address, as a `Source`. `engine::copy`
copies what a source holds into a place: it asks the source what it holds,
leaves alone a file that is already there with the same size and time,
fetches the rest, and says how far the bytes were checked.

`engine::copy` takes several sources that hold the same files, such as
several addresses for one file, and each chunk then comes from whichever
is free to send it. It does so only if a digest of every file is known.
Sources that give different sizes or different digests are refused, and so
are sources of which nothing tells that they hold the same bytes. What the
transfer notes beside a file is then kept under the digest, so it can be
carried on with from any of them.

An origin is found by asking for the first byte of the file. The answer
says how large the file is and what the server can do:

| The server gives | Read as | After a break | Run again |
| --- | --- | --- | --- |
| parts of the file, and a validator | several streams at once | carried on | carried on from the notes |
| the whole file only, and a validator | one stream | read again from the start | what was stored is kept, and the server sends it all again |
| no validator, but a digest of the file is known | one stream | read again from the start | begun again |
| no validator and no digest | one stream | the transfer ends | begun again |

A **validator** is what tells one version of a file from another: a strong
`ETag`, or else `Last-Modified`. It goes back with every request, as
`If-Range`, and every answer is held to what the first one said: its
version, its length, and which part of the file it is. An answer of another
version ends the transfer. Without a validator two answers could be two
files, so they are put together only if a digest of the whole file will be
checked at the end. The same rule covers two servers: bytes are put
together only when something ties them to one file.

A digest of the whole file is given by the user, or by the server in a
`Repr-Digest` or `Digest` header. With one the transfer is *verified*, and
without one it is *consistent*.

A request is for the rest of the file from where it starts, and carries a
run of chunks. A server that does not say how large its file is cannot be
used. Redirects are followed, a proxy named in the environment is used, and
certificates are checked against those the system trusts. `headers.rs`
reads what servers send, and is fuzzed.

## Sync

A sync makes a destination hold what a source holds. `model::mirror` works
out what to do, and touches no disk.

```
source tree ─┐
             ├─► pair by path ─► decide, for each path ─► plan: one step for each path
destination ─┘                     (proven)
```

`mirror::plan` goes through the two trees side by side, in the order of
their paths. For each path it finds what the two have there and whether
they hold the same, and `mirror::step::decide` answers with one step: add,
replace, touch, keep, remove or blocked.

`decide` is the part where a mistake loses data, so it is kept free of
paths, sizes and times, and is proven in Lean: with leave to remove, the
destination ends up with exactly what the source has; a step removes only
where the source has nothing; and where the two already match the step is
to keep, so a second sync does nothing. The pairing around it is checked by
property tests against a plain map from paths to what is at them.

Two files hold the same if they have the same size and were last changed in
the same second. Before two trees are compared, each is put as the local
system keeps it (`fs::as_kept`), so that what a system has no way to keep
is not found different on every run.

`mux::sync` carries a plan out on a local folder, in an order that loses
nothing:

```
look at both places ─► plan ─► stop if a rule forbids it
   ─► move to the trash what is removed, or is in the way of another kind
   ─► give each file that will be replaced a second name in the trash
   ─► copy what is added or replaced, and every directory (engine::copy over engine::part)
   ─► set the permissions of what is only touched
```

A file that is replaced keeps its name until the new one is renamed over
it, and its second name in the trash keeps it after that. `engine::part` is
a source that holds some of the entries of another, so the copy is the
ordinary one: part files, notes to carry on from, and a directory's time
put right after what was done inside it.

## Network

Each node has an Ed25519 key pair stored at `<config dir>/mux/node.key`.
The public key is the node id.

Connections are QUIC with TLS 1.3. There is no certificate authority: each
side presents a self-signed certificate that only carries its public key.
rustls verifies that the handshake was signed by that key, and the node id is
then read from the parsed certificate. `Endpoint::connect` fails unless the
peer's id is the one the caller asked for.

One UDP socket both dials and accepts, so the address a peer observes for a
node is also the address at which that node can be reached.

## Swarm

A `Node` owns the endpoint and a table of `Share`s. A share is one artifact
plus the bitfield of chunks held locally and a short-lived list of peers that
recently asked about it.

### Serving

Each inbound handshake and each request runs in its own task. A request is
one of: send the manifest, report status, or send a chunk. A status reply
contains the node's bitfield and the other participants it knows; asking for
status is also what registers the requester as a participant. This is the
whole of peer discovery: there is no tracker.

### Downloading

```
ticket ──► fetch manifest ──► verify against ticket ──► scan destination
                                                              │
        ┌─────────────────────────────────────────────────────┘
        ▼
   register share (start serving) ──► the engine's transfer ──► done
                                         │
                      one supplier for each peer
                      done: fsync the directories
```

`peer` knows the wire protocol and nothing about fetching. A peer is a
**supplier**: something that can be reached, asked which chunks it holds and
whom else it knows, and asked for a chunk. The engine does the rest, and
does the same for a supplier that is a web server or a folder.

Any `Source` is a supplier through `engine::supplier`. It keeps a reader
open after each chunk, and whoever asks for the chunk that follows reads on
from it. For a web server that is one request for a long run of chunks and
not one for each. A source that cannot be opened part of the way in is one
stream: the engine then runs a single pipeline over it, from the first
chunk that is missing.

## Transfer

`engine::transfer` fills a store from suppliers.

```
manager ──► one task per supplier, each with N pipelines
            pipeline: claim chunk → fetch → hand to the store
            store: hash → write → last chunk of a file → commit it
```

The manager keeps up to `max_suppliers` supplier tasks running and adds
suppliers that others tell of. What it does with a supplier follows from the
kind of failure the supplier reports:

| The supplier | The manager |
| --- | --- |
| cannot be reached, or breaks off | tries it again after a wait that doubles each time, up to 30 seconds. A supplier that was delivering starts again from the shortest wait. |
| sends a chunk that fails verification, or cannot be used at all | drops it for good |
| says it holds a chunk and then does not have it | stops asking it for that chunk |
| has nothing that is wanted, while another supplier waits for a turn | gives its turn to the one that has waited longest |

The last two rows and the order of turns exist because a property test
found transfers that stalled without them: see `engine/src/faults.rs`.

A transfer ends in one of four ways. Every chunk is stored. Or the store
fails, or what is being read changes, and the transfer ends there. Or
nothing arrives for the whole of the stall timeout. Or every supplier has
been dropped, so that nobody is left who could deliver: then it ends at
once, saying what the last one was dropped for, once the chunks that had
already arrived are stored.

A pipeline reserves a chunk with a `Claim`. Dropping a claim without
completing it returns the chunk to the pool, so cancellation and errors
cannot strand a chunk. The schedule records which supplier is fetching each
chunk. It is proven that only that supplier can settle the chunk, and that a
chunk becomes done in one way only: its fetcher reports it verified. Once a
chunk has arrived, storing it, counting it and marking it done are one step
that runs to its end whatever becomes of the supplier.

Pipelines start scanning at an offset derived from the ids of the two ends,
so ends that start together collect different chunks first and then
exchange them. A copy from a source has nobody to exchange with, and starts
at the first chunk.

A supplier is asked for its next chunk only when the store has taken the
last one. So the memory held for chunk data is bounded by
`max_suppliers × requests_per_supplier × chunk_size` (32 MiB with the
defaults), however slow the destination is.

Everything that happens is reported as a typed event: a supplier opened or
closed, a chunk stored, a file committed.

If the store fails, the transfer ends with that failure. The failure is
kept by the transfer itself and not only by whoever was waiting for the
chunk, because that waiter may have been stopped a moment before.

## Testing

- Unit tests sit beside the code they cover.
- `engine/src/faults.rs` runs transfers from made-up suppliers that lie,
  stall, break off and are slow, on a clock of its own. Whatever they do, a
  transfer ends with the right bytes or with an error.
- `web/tests/origin/` fetches from a web server written for the tests,
  which stops in the middle of a file, stops sending, sends the wrong part,
  and changes the file as it is read.
- `tests/connection.rs` checks mutual authentication and id pinning.
- `tests/download.rs` covers files, trees, resume, repair and error cases.
- `tests/swarm.rs` covers relaying, peer exchange and dishonest seeders.
- `tests/cli.rs` runs the compiled binary as separate processes, and kills
  a download at random moments to check that only whole files have their
  final names.
