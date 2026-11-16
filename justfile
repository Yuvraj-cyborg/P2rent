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

# Formatting, lints and documentation, with warnings as errors
lint:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets --locked -- -D warnings
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --document-private-items --locked

# Licences, known vulnerabilities, duplicate versions and unused dependencies
deps:
    cargo deny check
    cargo machete --with-metadata

# Spelling in code and documentation
spelling:
    typos

# Fail on functions that are near copies of each other. The detector compares
# shapes: a pasted function with small edits scores about 0.94, and unrelated
# functions that merely look alike score up to about 0.90, so 0.92 sits
# between them. It does not catch a copy in which every name was changed.
# Functions shorter than 8 lines are too small to judge.
dupes:
    similarity-rs crates xtask --threshold 0.92 --min-lines 8 --skip-test --fail-on-duplicates

# Check that every combination of features builds
features:
    cargo hack check --feature-powerset --all-targets --locked

# Run the tests that touch neither disk nor network under Miri, which
# detects undefined behaviour (needs a nightly toolchain with miri)
miri:
    cargo +nightly miri test --locked -p p2rent-core
    cargo +nightly miri test --locked -p p2rent-peer --lib -- swarm::ticket::

# The core crate holds the logic that gets proven. It must build with no
# standard library at all, and may depend only on the crates listed in xtask
# (needs the thumbv7em-none-eabihf target)
pure:
    cargo check --locked -p p2rent-core --no-default-features --target thumbv7em-none-eabihf
    cargo xtask layers

# Report binary size and dependency count, and fail if either is over budget
size:
    cargo xtask size

# Everything CI runs
ci: lint deps spelling dupes features miri pure size
    cargo test --workspace --locked

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
