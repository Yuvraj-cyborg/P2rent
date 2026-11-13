# Development tasks. Run `just` to list them.

default:
    @just --list

# Build a debug binary
build:
    cargo build

# Build an optimised binary at target/release/p2rent
release:
    cargo build --release --locked

# Run the test suite (extra arguments go to cargo test)
test *args:
    cargo test {{args}}

# Format the code
fmt:
    cargo fmt --all

# Check formatting and lints exactly as CI does
lint:
    cargo fmt --all -- --check
    cargo clippy --all-targets --locked -- -D warnings

# Everything CI runs
ci: lint
    cargo test --locked

# Time a loopback transfer, e.g. `just bench 4096 2` for 4 GiB from two seeders
bench size_mb="1024" seeders="1" runs="3" files="1":
    P2RENT_BENCH_SIZE_MB={{size_mb}} P2RENT_BENCH_SEEDERS={{seeders}} P2RENT_BENCH_RUNS={{runs}} P2RENT_BENCH_FILES={{files}} cargo bench --locked --bench transfer

# Run p2rent from source, e.g. `just run seed ./model.bin`
run *args:
    cargo run --release -- {{args}}

# Install p2rent into ~/.cargo/bin
install:
    cargo install --path . --locked

# Build with Nix and run the flake checks
nix:
    nix build
    nix flake check

# Seed a generated file and download it twice on this machine
demo size_mb="256": release
    #!/usr/bin/env bash
    set -euo pipefail
    bin="$PWD/target/release/p2rent"
    dir="$(mktemp -d)"
    trap 'kill $(jobs -p) 2>/dev/null || true; rm -rf "$dir"' EXIT
    head -c "$(( {{size_mb}} * 1024 * 1024 ))" /dev/urandom > "$dir/artifact.bin"

    P2RENT_KEY="$dir/origin.key" "$bin" seed "$dir/artifact.bin" --listen 127.0.0.1:0 > "$dir/ticket" &
    until [ -s "$dir/ticket" ]; do sleep 0.1; done
    ticket="$(cat "$dir/ticket")"

    P2RENT_KEY="$dir/first.key" "$bin" get "$ticket" --out "$dir/first" --listen 127.0.0.1:0 --seed &
    until [ -f "$dir/first/artifact.bin" ] && cmp -s "$dir/artifact.bin" "$dir/first/artifact.bin"; do sleep 0.2; done

    # The second download can pull from both the origin and the first node.
    P2RENT_KEY="$dir/second.key" "$bin" get "$ticket" --out "$dir/second" --listen 127.0.0.1:0
    cmp "$dir/artifact.bin" "$dir/second/artifact.bin" && echo "ok: all copies are identical"
