<div align="center">

<picture>
  <img src="./assets/logo.png" alt="p2rent Logo">
</picture>

<b>p2rent</b>: A blazing-fast peer-to-peer file sharing tool built from scratch in Rust.

<h3>
  <a href="#getting-started">Get Started</a> |
  <a href="#features">Features</a> |
  <a href="#how-it-works">How It Works</a> |
  <a href="#license">License</a>
</h3>

[![Built with Rust](https://img.shields.io/badge/built%20with-Rust-orange)](https://www.rust-lang.org/)
[![QUIC Protocol](https://img.shields.io/badge/protocol-QUIC-blue)](https://quicwg.org/)
[![License](https://img.shields.io/badge/license-MIT-green)](#license)

</div>

<hr>

**p2rent** is a mini BitTorrent-like file sharing system that leverages the modern QUIC protocol for fast, secure, and reliable peer-to-peer transfers.

No more slow uploads, complicated torrent clients, or centralized servers. Just share files directly between peers with end-to-end encryption and cryptographic integrity verification.

---

## Key Features

|                              |                                                                                                                                                      |
| ---------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------- |
| **QUIC Protocol**            | Built on Quinn (Rust QUIC implementation) for multiplexed, low-latency connections with built-in TLS 1.3 encryption.                                |
| **Cryptographic Security**   | Ed25519 keypairs for peer identity, Blake3 hashing for chunk integrity verification, and automatic certificate generation.                          |
| **Smart Chunking**           | Files are split into configurable chunks with content-addressed storage, enabling efficient parallel transfers and resumable downloads.             |
| **Parallel Processing**      | Optional parallel chunking using Rayon for blazing-fast preparation of large directories.                                                           |
| **Manifest System**          | JSON manifests track file metadata and chunk hashes, making it easy to verify integrity and resume interrupted transfers.                           |
| **Zero Configuration**       | Automatic keypair generation and storage, sensible defaults, and a simple CLI interface.                                                            |

---

## Getting Started

### Prerequisites

- Rust 1.75+ (2024 edition)
- Cargo package manager

### Installation

```bash
# Clone the repository
git clone https://github.com/yourusername/p2rent.git
cd p2rent

# Build the project
cargo build --release

# The binary will be at ./target/release/p2rent
```

### Quick Start

#### 1. Share a File or Directory

```bash
# Share a single file
p2rent share myfile.zip

# Share an entire directory (parallel processing)
p2rent share ./my-folder --parallel

# Custom chunk size (default: 1MB)
p2rent share largefile.iso --chunk-size 4194304
```

#### 2. Start a Server

```bash
# Start serving on default address (127.0.0.1:5000)
p2rent serve

# Or specify a custom address
p2rent serve --addr 0.0.0.0:5000
```

#### 3. Fetch from a Peer

```bash
# Fetch a file using its manifest
p2rent fetch --addr 192.168.1.10:5000 --manifest myfile.manifest.json

# Specify output path
p2rent fetch --addr peer:5000 --manifest file.manifest.json --out downloads/myfile.zip
```

---

## How It Works

```
┌─────────────┐     ┌──────────────┐     ┌─────────────┐
│   Share     │────▶│   Chunking   │────▶│   Storage   │
│   Command   │     │  + Hashing   │     │   + Index   │
└─────────────┘     └──────────────┘     └─────────────┘
                           │
                           ▼
                    ┌──────────────┐
                    │   Manifest   │
                    │    (JSON)    │
                    └──────────────┘
                           │
┌─────────────┐     ┌──────────────┐     ┌─────────────┐
│   Fetch     │◀────│    QUIC      │◀────│   Serve     │
│   Command   │     │   Transfer   │     │   Command   │
└─────────────┘     └──────────────┘     └─────────────┘
```

1. **Share**: Files are split into chunks, each hashed with Blake3 for integrity
2. **Manifest**: A JSON manifest is created containing file metadata and chunk hashes  
3. **Serve**: The server listens for incoming QUIC connections and handles chunk requests
4. **Fetch**: Clients request chunks using the manifest, verifying each chunk's hash
5. **Assemble**: Received chunks are combined back into the original file

---

## CLI Reference

```
p2rent - Mini BitTorrent-like sharing over QUIC

USAGE:
    p2rent <COMMAND>

COMMANDS:
    serve   Run a QUIC server and accept peers
    share   Prepare and share a file or directory (chunk + manifest + store)
    fetch   Fetch a file from a peer using a local manifest
    help    Print help information

OPTIONS:
    -h, --help       Print help
    -V, --version    Print version
```

### serve

```bash
p2rent serve [OPTIONS]

OPTIONS:
    --addr <ADDR>          Address to listen on [default: 127.0.0.1:5000]
    --storage-dir <DIR>    Directory to serve chunks from [default: chunks]
```

### share

```bash
p2rent share <PATH> [OPTIONS]

ARGUMENTS:
    <PATH>    Path to file or directory to share

OPTIONS:
    --chunk-size <BYTES>     Chunk size in bytes [default: 1048576]
    --manifest-dir <DIR>     Directory to write manifests [default: manifests]
    --storage-dir <DIR>      Directory to store chunks [default: chunks]
    --parallel               Process directory files in parallel
```

### fetch

```bash
p2rent fetch [OPTIONS]

OPTIONS:
    --addr <ADDR>           Peer address (e.g., 127.0.0.1:5000)
    --manifest <PATH>       Path to manifest JSON for the file to fetch
    --out <PATH>            Output file path
    --stem <NAME>           File stem (folder under storage_dir on the server)
```

---

## Architecture

```
src/
├── main.rs       # CLI entry point and command handlers
├── lib.rs        # Library exports
├── chunk.rs      # File chunking and reassembly
├── crypto.rs     # Ed25519 keypairs, signing, verification
├── error.rs      # Error types and handling
├── manifest.rs   # Manifest creation and parsing
├── protocol.rs   # Message types for peer communication
├── scanner.rs    # Directory scanning utilities
├── storage.rs    # Chunk storage and retrieval
├── sync.rs       # Synchronization primitives
└── net/
    ├── mod.rs    # Network module exports
    └── quic.rs   # QUIC client/server implementation
```

---

## Security

- **Ed25519 Signatures**: Peer identity is verified using Ed25519 digital signatures
- **Blake3 Hashing**: Every chunk is hashed with Blake3, ensuring data integrity
- **TLS 1.3**: All QUIC connections use TLS 1.3 encryption by default
- **Local Keypair Storage**: Keys are stored in `~/.config/p2rent/` with restricted permissions (0600)

---

## Performance

p2rent is designed for high throughput:

- **Streaming chunking** with configurable buffer sizes
- **Parallel directory processing** via Rayon
- **Multiplexed QUIC streams** for concurrent chunk transfers
- **Content-addressed storage** enabling deduplication

---

## Roadmap

- [ ] DHT for peer discovery
- [ ] Multi-peer parallel downloads (swarm)
- [ ] NAT traversal / hole punching
- [ ] Bandwidth throttling
- [ ] Web UI for monitoring
- [ ] Resume interrupted transfers
- [ ] Selective file downloading from directories

---

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.

---

## License

p2rent is licensed under the MIT License.

---

<div align="center">

**Share files, not worries.**

</div>
