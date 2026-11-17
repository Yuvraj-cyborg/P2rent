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
│   ├── artifact/      hash.rs  manifest.rs  path.rs  layout.rs  bitfield.rs  wire.rs
│   ├── assembly.rs
│   ├── schedule.rs
│   ├── location.rs
│   ├── place.rs
│   ├── retry.rs
│   ├── ticket.rs
│   └── hex.rs
├── engine/src/        store.rs  transfer.rs  claims.rs  conformance.rs  faults.rs
├── fs/
│   ├── src/           index.rs  folder.rs  io.rs
│   └── tests/folder/  what a store over a folder leaves on disk
├── peer/
│   ├── src/
│   │   ├── net/       identity.rs  tls.rs  endpoint.rs  proto.rs
│   │   ├── swarm/     node.rs  share.rs  server.rs
│   │   │   └── download/  mod.rs  supplier.rs
│   └── tests/         connection.rs  download.rs  swarm.rs  properties.rs
└── cli/
    ├── src/           lib.rs  main.rs  cli/
    └── tests/         cli.rs
benches/               instructions.rs  transfer.rs (see docs/performance.md)
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
| `Source` | Reads a range of bytes of one file. |
| `Sink` | Makes the place ready and says which chunks it already holds; writes bytes at an offset; commits a file; aborts a file; finishes the tree. |
| `Supplier` | Is reached, says which chunks it holds and which other suppliers it knows of, and hands over a chunk. |

A file being written is kept apart from the file under its final name until
it is committed. A failure says which of four things it is (the place is
unreachable, it does not hold what was asked for, it answered with something
it must not, or trying again gains nothing), so the engine never has to know
what a socket or a disk is.

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
would do the same for a supplier that is a web server or a folder.

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
| sends a chunk that fails verification | drops it for good |
| says it holds a chunk and then does not have it | stops asking it for that chunk |
| has nothing that is wanted, while another supplier waits for a turn | gives its turn to the one that has waited longest |

The last two rows and the order of turns exist because a property test
found transfers that stalled without them: see `engine/src/faults.rs`.

A pipeline reserves a chunk with a `Claim`. Dropping a claim without
completing it returns the chunk to the pool, so cancellation and errors
cannot strand a chunk. The schedule records which supplier is fetching each
chunk. It is proven that only that supplier can settle the chunk, and that a
chunk becomes done in one way only: its fetcher reports it verified. Once a
chunk has arrived, storing it, counting it and marking it done are one step
that runs to its end whatever becomes of the supplier.

Pipelines start scanning at an offset derived from the ids of the two ends,
so ends that start together collect different chunks first and then
exchange them.

A supplier is asked for its next chunk only when the store has taken the
last one. So the memory held for chunk data is bounded by
`max_suppliers × requests_per_supplier × chunk_size` (32 MiB with the
defaults), however slow the destination is.

Everything that happens is reported as a typed event: a supplier opened or
closed, a chunk stored, a file committed. A finished transfer says how far
its bytes were checked. Today that is always *verified*: the digest of every
chunk was known before the transfer began.

## Testing

- Unit tests sit beside the code they cover.
- `engine/src/faults.rs` runs transfers from made-up suppliers that lie,
  stall, break off and are slow, on a clock of its own. Whatever they do, a
  transfer ends with the right bytes or with an error.
- `tests/connection.rs` checks mutual authentication and id pinning.
- `tests/download.rs` covers files, trees, resume, repair and error cases.
- `tests/swarm.rs` covers relaying, peer exchange and dishonest seeders.
- `tests/cli.rs` runs the compiled binary as separate processes, and kills
  a download at random moments to check that only whole files have their
  final names.
