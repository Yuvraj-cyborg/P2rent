# Architecture

p2rent is one crate with a library and a thin binary. The library is split
into four layers; each depends only on the ones above it in this table.

| Layer | Path | Responsibility | Knows about |
| --- | --- | --- | --- |
| Data model | `src/artifact/` | Hashes, manifests, chunk layout, bitfields | nothing else |
| Storage | `src/store/` | Hashing files into artifacts; reading and writing chunks in place | artifact |
| Network | `src/net/` | Node identity, authenticated QUIC, wire messages | artifact |
| Swarm | `src/swarm/` | What a node serves, how it answers peers, multi-source download | all of the above |
| CLI | `src/cli/`, `src/main.rs` | Argument parsing, progress bars, output | the library's public API |

```
src/
├── artifact/      hash.rs  manifest.rs  bitfield.rs
├── store/         index.rs  disk.rs  io.rs
├── net/           identity.rs  tls.rs  endpoint.rs  proto.rs
├── swarm/         node.rs  share.rs  server.rs  ticket.rs
│   └── download/  mod.rs  session.rs  peer.rs
├── cli/           seed.rs  get.rs  ui.rs
├── error.rs
├── lib.rs
└── main.rs
tests/             connection.rs  download.rs  swarm.rs  cli.rs
```

## Data model

An **artifact** is a file or a directory tree. Its **manifest** lists every
file (relative path, size, execute bit) and the BLAKE3 digest of every chunk.
The **artifact id** is a domain-separated BLAKE3 hash of the encoded manifest.

Files are chunked independently at a fixed size, so chunk *n* of a file always
starts at byte *n × chunk size*. Two consequences follow:

- a seeder can serve any chunk with one positional read from the original
  file, with no separate chunk store;
- a downloader can write chunks in any order into pre-sized files.

Manifests come from untrusted peers. `Manifest::validate` runs before a
manifest is used for anything and rejects path components that are empty,
`.`, `..`, or contain a separator, drive colon or NUL; paths that are unsorted,
duplicated or that name something as both a file and a directory; and chunk
counts that disagree with the declared file sizes.

## Storage

`store::index` walks a path, hashes chunks in parallel and returns an
artifact. `DiskStore` maps a chunk index to a file and offset and performs
positional reads and writes, so many chunks can be in flight without sharing
a file cursor.

Resuming needs no state file. `DiskStore::scan` re-hashes whatever is at the
destination and reports which chunks are already valid; anything else is
fetched again. This also repairs files that were corrupted after a download.

## Network

Each node has an Ed25519 key pair stored at `<config dir>/p2rent/node.key`.
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
   register share (start serving) ──► manager ──► finalize (fsync, modes)
                                         │
                      one task per peer, each with N workers
                      worker: claim chunk → fetch → hash → write → mark
```

The manager keeps up to `max_peers` peer tasks running, adds peers reported
through status replies, retries failed peers with exponential backoff and
permanently drops any peer whose data fails verification.

A worker reserves a chunk with a `Claim`. Dropping a claim without completing
it returns the chunk to the pool, so cancellation and errors cannot strand a
chunk. Workers start scanning at an offset derived from the two node ids, so
simultaneous downloaders collect different chunks first and then exchange
them.

Memory held for chunk data is bounded by
`max_peers × requests_per_peer × chunk_size` (32 MiB with the defaults).

## Testing

- Unit tests sit beside the code they cover.
- `tests/connection.rs` checks mutual authentication and id pinning.
- `tests/download.rs` covers files, trees, resume, repair and error cases.
- `tests/swarm.rs` covers relaying, peer exchange and dishonest seeders.
- `tests/cli.rs` runs the compiled binary as separate processes.
