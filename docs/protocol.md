# Protocol

This describes protocol version 4, negotiated with the ALPN token `mux/4`.
Version 3 carried the same messages over connections authenticated with
certificates, and version 2 was that protocol under the project's former
name, p2rent, with the token `p2rent/2`. No two versions interoperate.

## Tickets

```
mux://<artifact id>?peer=<node id>[@<where>]&peer=...&url=<web address>&url=...
```

- `artifact id`: 64 hex characters. It pins the content and the metadata of
  one artifact. [Identifiers](#identifiers) says how it is derived.
- `node id`: 64 hex characters, an Ed25519 public key.
- `where` is `host:port`, or the address of a relay beginning `https://` or
  `http://`. `host` may be an IPv4 address, a bracketed IPv6 address or a
  DNS name. Without it the node is looked up by its id. A node reached at
  several places has one `peer=` for each.
- `url` is a web address said to hold the same bytes, for an artifact that
  is one file. In it `%`, `&`, `#` and `?` are written `%25`, `%26`, `%23`
  and `%3F`, so that it ends where the next parameter begins. It is one
  more place to fetch chunks from and is trusted no more than a peer:
  every chunk is compared with the digest the manifest gives for it.

Unknown query parameters are ignored, so a build that does not know `url`
fetches from the peers alone.

## Transport and identity

QUIC with TLS 1.3 only, as [iroh](https://github.com/n0-computer/iroh)
makes it. Each side presents its Ed25519 public key in place of a
certificate ([RFC 7250](https://www.rfc-editor.org/rfc/rfc7250)), and the
handshake signature must verify under that key. A node is dialled by its
id, and the handshake fails if whoever answers holds another key.

Nodes accept datagrams of up to 1472 bytes. Connections start at QUIC's
1200-byte minimum and grow only as far as path MTU discovery confirms.

## Framing

Every request uses its own bidirectional stream.

```
requester:  frame(Request)                    then FIN
responder:  frame(Response) [payload bytes]   then FIN
```

A frame is a big-endian `u32` length followed by that many bytes of
[postcard](https://postcard.jamesmunns.com/wire-format)-encoded message.
Request frames are limited to 1 KiB and response frames to 4 MiB.

## Messages

| Request | Response | Payload |
| --- | --- | --- |
| `Manifest { artifact }` | `Manifest { len }` | `len` bytes of encoded manifest, at most 64 MiB |
| `Status { content }` | `Status { have, peers }` | none |
| `Chunk { content, index }` | `Chunk { len }` | `len` bytes of chunk data |

Any request may be answered with `NotFound`.

A manifest is asked for by artifact id. Chunks, and who holds them, are
asked for by content id. Two nodes that hold the same bytes under different
names, permissions or times have different manifests and the same content
id, so they are one swarm: either can supply the other's downloaders with
chunks. A node answers a `Manifest` request only for a manifest it has, byte
for byte.

- `have` is `All` or a bitfield with one bit per chunk, least significant bit
  first within each byte.
- `peers` lists up to 32 other participants, most recently seen first. Each
  is `(node id, socket addresses, relay)`. A participant is any node that
  sent a `Status` request for the content in the last two minutes. The
  addresses and the relay are those its connection was seen to go by, and
  either may be missing: the node id is always enough to try.
- A `Chunk` response whose length differs from the length implied by the
  manifest is a protocol error.

## Manifest

This is manifest format version 1.

```
Manifest { content: Content, metadata: Metadata }

Content {
    chunk_size: u32            // 4 KiB ..= 16 MiB
    entries:    [Entry]        // the root first, then the rest sorted by path
    chunks:     [32 bytes]     // chunk digests of every file, in entry order
}
Entry { path: string, kind: File { size: u64 } | Directory | Symlink { target: string } }

Metadata {
    name:    string            // what the root is stored under, one name
    entries: [EntryMetadata]   // one for each entry of the content, in order
}
EntryMetadata { mode: u16, modified: optional Timestamp }
Timestamp { seconds: i64, nanos: u32 }   // from 1970-01-01 00:00:00 UTC
```

The content is what the artifact *is*. The metadata is everything else that
is recorded about it. They are kept apart so that the same bytes are the same
swarm whatever they are called and whenever they were last touched.

### Encoding

```
byte 0        format version, 1
bytes 1..9    length of the encoded content, little-endian u64
then          the content, postcard-encoded
then          the metadata, postcard-encoded
```

A receiver refuses a manifest whose first byte is a version it does not
know, and says which version it was. It refuses bytes that are cut short and
bytes with anything left over.

### Identifiers

Both are BLAKE3 in key-derivation mode. Each context names the format, so an
identifier of one kind or one format cannot be taken for another.

| Identifier | Derived from | Under the context |
| --- | --- | --- |
| content id | the encoded content | `mux 2026 content v1` |
| artifact id | the byte `1`, then the content id, then the encoded metadata | `mux 2026 manifest v1` |

A ticket carries the artifact id, so it pins the metadata as well as the
content. Changing only a name, a permission or a time changes the artifact
id and leaves the content id as it was.

Receivers check that a manifest's bytes give the artifact id they asked for
before decoding them, and validate the decoded manifest before using it.

### Entries

The first entry is the root, with the empty path. It is a file or a
directory. If it is a file, it is the only entry.

Every other entry has a path relative to the root, with `/` separators.

- Each name in a path, and the artifact's name, is at most 246 bytes, is not
  empty, `.` or `..`, and contains no `/`, `\`, `:` or NUL. It also does not
  end in `.mux-part`, in any letter case: that suffix names a file that is
  still being downloaded, and 246 bytes leaves room for it within the 255
  that most file systems allow.
- Entries are in the order of their paths as byte strings, and each path is
  greater than the one before it. So no path appears twice.
- The directory that holds an entry is itself an entry, of kind `Directory`.
  Every directory is listed, which is how an empty one survives. It also
  means nothing can lie under a file or behind a link.
- A link's target is relative and at most 4096 bytes: names joined by `/`,
  each a name as above or `..`. Followed from the directory that holds the
  link, it never climbs above the root. A link whose target does not exist is
  allowed. A link is recorded as a link and is never followed.

A file of `size` bytes contributes `ceil(size / chunk_size)` digests; the
last chunk of a file may be short. A directory or a link contributes none.
Chunk digests are plain BLAKE3 hashes of chunk data.

### Metadata

`mode` holds permission bits as Unix has them: read, write and execute for
the owner, the group and everyone else. No other bit may be set, so a
manifest cannot make a file set-user-id. `nanos` is below one thousand
million. A link's own mode and time are not restored, and are recorded as
`0o777` and none.

On Windows a file with no write bit is made read-only, and nothing else is
applied. Links are not created there, and the download reports how many it
left out.

### Names: letter case and Unicode

A name is a string of bytes, valid as UTF-8. It is stored, compared and
sorted byte for byte. Nothing is normalised and no letter case is changed:
what was indexed is what arrives.

So `README` and `readme` are two entries, and so are a name with a precomposed
`é` and the same name with `e` followed by a combining accent. Linux file
systems hold both. Windows and the usual macOS file systems treat each pair
as one name. A download to such a file system checks for this before writing
anything, and fails with an error naming the two paths. It never writes two
entries into one file.

## Verification rules

A downloader must hash every chunk and compare it with the manifest before
writing it. On a mismatch it discards the data and stops using that peer.

A downloader gives a file its final name only when every chunk of the file
has passed that check.

A downloader never writes through a link. Where the manifest lists a
directory, a link found there is an error. Where it lists a file or a link,
whatever is there is replaced by a rename, not opened and written.
