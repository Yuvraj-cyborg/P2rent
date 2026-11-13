# Protocol

This describes protocol version 2, negotiated with the ALPN token `p2rent/2`.

## Tickets

```
p2rent://<artifact id>?peer=<node id>@<host:port>&peer=...
```

- `artifact id`: 64 hex characters, the BLAKE3 hash of the encoded manifest
  under the derivation context `p2rent 2026 manifest v1`.
- `node id`: 64 hex characters, an Ed25519 public key.
- `host` may be an IPv4 address, a bracketed IPv6 address or a DNS name.

Unknown query parameters are ignored.

## Transport and identity

QUIC with TLS 1.3 only. Both sides present a self-signed X.509 certificate
whose subject public key is their Ed25519 key. The handshake signature must
verify under that key; no other property of the certificate is checked. The
dialling side closes the connection if the peer's key is not the node id it
expected.

Nodes accept datagrams of up to 8952 bytes, which is what a 9000-byte jumbo
frame holds after the IPv6 and UDP headers. Connections start at QUIC's
1200-byte minimum and grow only as far as path MTU discovery confirms, so a
standard 1500-byte route behaves as before.

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
| `Status { artifact }` | `Status { have, peers }` | none |
| `Chunk { artifact, index }` | `Chunk { len }` | `len` bytes of chunk data |

Any request may be answered with `NotFound`.

- `have` is `All` or a bitfield with one bit per chunk, least significant bit
  first within each byte.
- `peers` lists up to 32 other participants as `(node id, socket address)`,
  most recently seen first. A participant is any node that sent a `Status`
  request for the artifact in the last two minutes; the address is the one
  observed on that connection.
- A `Chunk` response whose length differs from the length implied by the
  manifest is a protocol error.

## Manifest

```
Manifest {
    name:       string        // artifact root name, one path component
    kind:       File | Directory
    chunk_size: u32           // 4 KiB ..= 16 MiB
    files:      [FileEntry]   // sorted by path
    chunks:     [32 bytes]    // per-file chunk digests, in file order
}
FileEntry { path: string, size: u64, executable: bool }
```

Paths use `/` separators and are relative to the artifact root. A file of
`size` bytes contributes `ceil(size / chunk_size)` digests; the last chunk of
a file may be short. Chunk digests are plain BLAKE3 hashes of chunk data.

Receivers verify that the manifest bytes hash to the artifact id before
decoding them, and validate the decoded manifest before using it.

## Verification rules

A downloader must hash every chunk and compare it with the manifest before
writing it. On a mismatch it discards the data and stops using that peer.
