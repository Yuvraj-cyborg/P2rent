# Architecture

mux is built from four crates. Each depends only on the ones above it in
this table.

| Crate | Responsibility | Knows about |
| --- | --- | --- |
| `model` | The data model and the logic that is proven: hashes, manifests, chunk layout, bitfields, the schedule of which chunk to fetch next, and how a place is named: locations and tickets | nothing else |
| `fs` | Local folders: hashing files into artifacts; reading chunks from them; writing chunks to part files that are renamed into place when whole | model |
| `peer` | Node identity, authenticated QUIC, wire messages, and the swarm: what a node serves, how it answers peers, multi-source download | model, fs |
| `cli` | The `mux` command (argument parsing, progress bars, output) and the `mux` library, which re-exports the three above | all of the above |

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
│   ├── ticket.rs
│   └── hex.rs
├── fs/src/            index.rs  disk.rs  io.rs
├── peer/
│   ├── src/
│   │   ├── net/       identity.rs  tls.rs  endpoint.rs  proto.rs
│   │   ├── swarm/     node.rs  share.rs  server.rs
│   │   │   └── download/  mod.rs  session.rs  claims.rs  peer.rs
│   │   └── sync.rs    locks that recover from poisoning
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

## Storage

`store::index` walks a path, hashes chunks in parallel and returns an
artifact. `DiskStore` maps a chunk index to a file and offset and performs
positional reads and writes, so many chunks can be in flight without sharing
a file cursor.

A file under its final name is always whole and correct. While a file is
being downloaded its chunks go into a part file beside it, `<name>.mux-part`.
`DiskStore::write_chunk` checks a chunk against the manifest before writing
it, and when the last chunk of a file is in place it flushes the part file
and renames it over the final name. The directories are flushed once, when
the download completes, so that the renames survive a power cut. So however the
program stops, there is never a file with holes under a name a user would
open, and a file that was already there is replaced in one step or left
alone, never changed in place. The decision "this file is now whole" is made
by `model::assembly`, which is proven: a file is announced only when every
chunk of it is in place.

Chunks are read from part files as well as whole ones, so a node serves what
it holds while it downloads the rest.

Directories are created first, and each must be a real directory: a link
standing where one belongs is an error, so nothing is ever written through a
link to somewhere else. Links are created by the same rename as files. A
file gets its recorded time and permissions just before its rename. A
directory gets them when the download completes, the deepest first, because
filling a directory changes its time and a read-only one cannot be filled.

Resuming needs no state file. `DiskStore::resume` re-hashes whatever is at
the destination. A whole file is kept. A part file keeps every chunk that is
right. A file under its final name that is not whole, because it was damaged
or is another version, is left as it is until its replacement is ready, and
the chunks of it that are right are copied into the part file instead of
being fetched again. Indexing leaves part files out.

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
   register share (start serving) ──► manager ──► done
                                         │
                      one task per peer, each with N workers
                      worker: claim chunk → fetch → hash → write to part file
                              last chunk of a file → fsync, rename into place
                      done: fsync the directories
```

The manager keeps up to `max_peers` peer tasks running, adds peers reported
through status replies, retries failed peers with exponential backoff and
permanently drops any peer whose data fails verification.

A worker reserves a chunk with a `Claim`. Dropping a claim without completing
it returns the chunk to the pool, so cancellation and errors cannot strand a
chunk. The schedule records which connection is fetching each chunk. It is
proven that only that connection can settle the chunk, and that a chunk
becomes done in one way only: its fetcher reports it verified. Workers start scanning at an offset derived from the two node ids, so
simultaneous downloaders collect different chunks first and then exchange
them.

Memory held for chunk data is bounded by
`max_peers × requests_per_peer × chunk_size` (32 MiB with the defaults).

## Testing

- Unit tests sit beside the code they cover.
- `tests/connection.rs` checks mutual authentication and id pinning.
- `tests/download.rs` covers files, trees, resume, repair and error cases.
- `tests/swarm.rs` covers relaying, peer exchange and dishonest seeders.
- `tests/cli.rs` runs the compiled binary as separate processes, and kills
  a download at random moments to check that only whole files have their
  final names.
