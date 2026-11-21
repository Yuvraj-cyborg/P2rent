# How mux works

A picture-first walk through one artifact, from `seed` to a finished swarm.
For module layout see [architecture](architecture.md); for the bytes on the
wire see [protocol](protocol.md).

## The problem

When many machines download the same thing from one place, they split that
place's uplink between them. Ten machines each get a tenth.

```
 direct download                    mux

      source                        source ─── A ─── C
   ┌──┬──┼──┬──┐                       │   ╲   │   ╱ │
   A  B  C  D  E                       B ───── D ─── E

 five machines share one uplink     every machine is also a source
```

In a swarm, a machine starts serving a chunk the moment it has verified it.
Adding machines adds upload capacity, so the rollout does not slow down as the
fleet grows. A swarm is asked for, with `--swarm`: without it a ticket is
fetched from the machine that offers it and from nowhere else.

## 1. Seeding turns files into a ticket

`mux seed` reads the files once, splits each into fixed-size chunks and
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

## 2. The ticket says what, who and where, and lets its holder in

```
mux://450b9c6b…6d2bfc54?secret=24591239…55097e5f&peer=35964d95…f14dd0e7@10.0.0.5:4433
         └───────┬───────┘        └───────┬───────┘      └───────┬───────┘ └─────┬─────┘
           what to fetch          what lets you ask        who to trust    where they are
      (hash of the manifest)       (16 random bytes)     (node public key)
```

- **What**: the id pins the manifest, and the manifest pins every chunk. A
  ticket can only ever produce one exact set of bytes.
- **The secret**: made new for each offer. A machine that offers answers
  only who proves it knows the secret, so the ticket is the key to the
  offer: pass it on as you would a password.
- **Who**: the downloader refuses to talk to anything at that address that
  cannot prove it holds this key.
- **Where**: a starting point only. In a swarm, other peers are learned
  from it.

### Who is answered

```
                        ┌─ cannot prove it knows the secret ──▶ "nothing here"
 a machine asks ──▶ ────┤
                        │                    ┌─ no ──▶ "not for you" (exit 7)
                        └─ proves it ─ let in? ┤
                                             └─ yes ─▶ the manifest, the chunks
```

The proof is BLAKE3 keyed with the secret over the ids of the two machines,
which the connection has already proven. The secret itself is never sent,
and a proof is of no use to any other pair of machines.

Who is let in is the sender's choice:

| `mux cp ./weights peer:` | Who may fetch |
| --- | --- |
| nothing more | whoever has the ticket |
| `--to NODE` | only that machine: `mux id` prints its id there |
| `--once` | only the first machine that asks |
| `--swarm` | whoever has the ticket, from the sender and from each other |

Stopping the sender ends the offer. Offering the same files again makes a
new secret, so an old ticket opens nothing.

## 3. A swarm fills in

This is what `--swarm` asks for. Without it, a machine that fetches serves
nothing and is told of to nobody, and the picture is one sender and its
receivers.

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
  origin, which tells every caller of a swarm who else serves it. Each
  shows the others the same proof it showed the origin: a machine without
  the ticket gets nothing from any of them.
- **t3**: between them the three hold every chunk, so the origin can leave and
  the download still finishes.

## 4. Nothing unverified reaches the disk

```
                                           ┌─ equal ────▶ write to the part file,
 peer ── chunk 17 ──▶ BLAKE3 ──▶ compare ──┤              offer it to others
                                 with the  │
                                 manifest  └─ different ▶ discard, ban the peer,
                                                          ask someone else
```

Integrity does not depend on trusting peers. A peer that lies wastes one
request and is never contacted again.

A file is written as `<name>.mux-part` and gets its real name only when every
chunk of it has been verified:

```
 downloading   model.bin.mux-part   ██████░░██░░░░░░
 last chunk    model.bin.mux-part   ████████████████   flush to disk
 whole         model.bin            ████████████████   renamed in one step
```

So a file under its real name is always complete and correct, however the
download ended. A file that was already there is replaced by that one rename
or left alone, never changed in place.

## 5. Resume is a re-hash

There is no state file. On start, `get` hashes whatever is already in the
destination and requests only what does not match.

```
 after a crash     ██████░░██░░░░░░     re-hash the part file
 run `get` again   ██████▒▒██▒▒▒▒▒▒     fetch only the gaps
```

The same mechanism repairs a damaged copy or updates an old one: point `get`
at it, and the chunks that are already right are copied from it instead of
being fetched. The old file stays as it is until the new one is whole.

## What this buys as the fleet grows

This is a model, not a measurement. mux has so far been run with three
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
