# How p2rent works

A picture-first walk through one artifact, from `seed` to a finished swarm.
For module layout see [architecture](architecture.md); for the bytes on the
wire see [protocol](protocol.md).

## The problem

When many machines download the same thing from one place, they split that
place's uplink between them. Ten machines each get a tenth.

```
 direct download                    p2rent

      source                        source ─── A ─── C
   ┌──┬──┼──┬──┐                       │   ╲   │   ╱ │
   A  B  C  D  E                       B ───── D ─── E

 five machines share one uplink     every machine is also a source
```

In a swarm, a machine starts serving a chunk the moment it has verified it.
Adding machines adds upload capacity, so the rollout does not slow down as the
fleet grows.

## 1. Seeding turns files into a ticket

`p2rent seed` reads the files once, splits each into fixed-size chunks and
hashes every chunk. The list of files and chunk hashes is the manifest. The
hash of the manifest is the artifact id.

```
 weights/                     manifest
 ├── config.json    ──┐    ┌───────────────────────┐
 ├── model-00.bin   ──┼──▶ │ files:  path, size    │ ──BLAKE3──▶ artifact id
 └── model-01.bin   ──┘    │ chunks: hash, hash, … │
                           └───────────────────────┘
```

Nothing is copied. The seeder serves straight from the original files.

## 2. The ticket says what, who and where

```
p2rent://450b9c6b…6d2bfc54?peer=35964d95…f14dd0e7@10.0.0.5:4433
         └───────┬───────┘      └───────┬───────┘ └─────┬─────┘
           what to fetch          who to trust    where they are
      (hash of the manifest)    (node public key)
```

- **What**: the id pins the manifest, and the manifest pins every chunk. A
  ticket can only ever produce one exact set of bytes.
- **Who**: the downloader refuses to talk to anything at that address that
  cannot prove it holds this key.
- **Where**: a starting point only. Other peers are learned from the swarm.

## 3. The swarm fills in

Each row is a moment in time. Each bar is one machine's copy of an
eight-chunk artifact.

```
 t0   origin ████████    A ░░░░░░░░    B ░░░░░░░░    C ░░░░░░░░
 t1   origin ████████    A ██░░░░░░    B ░░█░░░░░    C ░░░░░██░
 t2   origin ████████    A ████░█░░    B ░░███░█░    C █░░░░███
 t3   origin  offline    A ████████    B ███████░    C █░██████
```

- **t1**: the origin hands different chunks to different machines. Each
  downloader starts scanning from a different position so they do not all ask
  for chunk 0 first.
- **t2**: A, B and C trade with each other. They found each other through the
  origin, which tells every caller who else has asked recently.
- **t3**: between them the three hold every chunk, so the origin can leave and
  the download still finishes.

## 4. Nothing unverified reaches the disk

```
                                           ┌─ equal ────▶ write at its offset,
 peer ── chunk 17 ──▶ BLAKE3 ──▶ compare ──┤              offer it to others
                                 with the  │
                                 manifest  └─ different ▶ discard, ban the peer,
                                                          ask someone else
```

Integrity does not depend on trusting peers. A peer that lies wastes one
request and is never contacted again.

## 5. Resume is a re-hash

There is no state file. On start, `get` hashes whatever is already in the
destination and requests only what does not match.

```
 after a crash     ██████░░██░░░░░░     re-hash what is on disk
 run `get` again   ██████▒▒██▒▒▒▒▒▒     fetch only the gaps
```

The same mechanism repairs a damaged copy: point `get` at it and only the bad
chunks are replaced.

## What this buys as the fleet grows

This is a model, not a measurement. p2rent has so far been run with three
nodes on one machine.

Assume a 50 GB artifact and a 1 Gbit/s link on every machine, source included.

| Machines | Direct, one source | Swarm, best case | Served by the source: direct | Served by the source: swarm, at least |
| ---: | ---: | ---: | ---: | ---: |
| 10 | 1 h 7 min | about 7 min | 500 GB | 50 GB |
| 100 | 11 h | about 7 min | 5 TB | 50 GB |
| 500 | 56 h | about 7 min | 25 TB | 50 GB |

- **Direct** is `machines × size ÷ source uplink`: everyone waits on one link.
- **Swarm, best case** is the textbook lower bound for peer-assisted
  distribution: the largest of `size ÷ source uplink`, `size ÷ slowest
  download link`, and `machines × size ÷ total upload capacity`. A real swarm
  finishes above this bound.

The shape matters more than the exact numbers: direct time grows with every
machine added, swarm time stays roughly flat, and the source ships about one
copy instead of one per machine.

It does not help when there is one downloader, when the source link is already
faster than the downloaders can write, or when machines cannot reach each
other.
