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
    cargo test --locked --workspace {{args}}

# Format the code
fmt:
    cargo fmt --all

# ---- What CI runs. Keep these in step with .github/workflows/ci.yml ----

# Everything CI runs
ci: lint test msrv audit features miri

# Formatting, lints, documentation and spelling, with warnings as errors
lint:
    cargo fmt --all -- --check
    cargo clippy --locked --workspace --all-targets -- -D warnings
    RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --no-deps --document-private-items
    typos

# Needs the 1.88 toolchain: `rustup toolchain install 1.88`.

# Check that the code builds with the oldest supported compiler
msrv:
    cargo +1.88 check --locked --workspace --all-targets

# Licences, known vulnerabilities and duplicate versions of dependencies
audit:
    cargo deny check

# Check that every combination of features builds
features:
    cargo hack check --locked --feature-powerset --all-targets

# Miri detects undefined behaviour. Needs a nightly toolchain with miri.

# Run the tests that touch neither disk nor network under Miri
miri:
    cargo +nightly miri test --locked -p p2rent-core
    cargo +nightly miri test --locked -p p2rent-peer --lib -- swarm::ticket::

# ---- Our own analysis. Not part of CI: run it before opening a pull request ----

# Every local analysis check
analysis: unused dupes pure deps size

# Dependencies that are declared but never used
unused:
    cargo machete --with-metadata

# The detector compares shapes: a pasted function with small edits scores
# about 0.94, and unrelated functions that merely look alike score up to
# about 0.90, so 0.92 sits between them. It does not catch a copy in which
# every name was changed. Functions shorter than 8 lines are too small to
# judge.

# Fail on functions that are near copies of each other
dupes:
    similarity-rs crates xtask --threshold 0.92 --min-lines 8 --skip-test --fail-on-duplicates

# The core crate holds the logic that gets proven. It must build with no
# standard library at all, and may depend only on the crates listed in xtask.
# Needs the thumbv7em-none-eabihf target.

# Check that the core crate has no I/O and no unexpected dependency
pure:
    cargo check --locked -p p2rent-core --no-default-features --target thumbv7em-none-eabihf
    cargo xtask layers

# After adding or dropping a dependency, run `cargo xtask deps --write`, so
# that the change shows the crate's name in its diff.

# Check the committed list of crates that ship in the released binaries
deps:
    cargo xtask deps

# Report the size of the release binary and fail if it is over budget
size:
    cargo xtask size

# CI's checks and our own analysis together
all: ci analysis

# Time a loopback transfer, e.g. `just bench 4096 2` for 4 GiB from two seeders
bench size_mb="1024" seeders="1" runs="3" files="1":
    P2RENT_BENCH_SIZE_MB={{size_mb}} P2RENT_BENCH_SEEDERS={{seeders}} P2RENT_BENCH_RUNS={{runs}} P2RENT_BENCH_FILES={{files}} cargo bench --locked -p p2rent-peer --bench transfer

# Run p2rent from source, e.g. `just run seed ./model.bin`
run *args:
    cargo run --release -p p2rent -- {{args}}

# Install p2rent into ~/.cargo/bin
install:
    cargo install --path crates/p2rent --locked

# Build with Nix and run the flake checks
nix:
    nix build
    nix flake check
