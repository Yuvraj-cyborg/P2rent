# Development tasks. Run `just` to list them.

default:
    @just --list

# Build a debug binary
build:
    cargo build

# Build an optimised binary at target/release/mux
release:
    cargo build --release --locked

# Run the test suite (extra arguments go to nextest), then the doc tests
test *args:
    cargo nextest run --locked --workspace {{args}}
    cargo test --locked --workspace --doc

# Format the code
fmt:
    cargo fmt --all
    cargo fmt --all --manifest-path fuzz/Cargo.toml

# ---- What CI runs. Keep these in step with .github/workflows/ci.yml ----

# Everything CI runs
ci: lint test msrv audit features miri fuzz loom

# `just bench` is a CI job too, but it needs Valgrind, which runs on Linux
# only, so it is not part of `just ci`.

# Formatting, lints, documentation and spelling, with warnings as errors
lint:
    cargo fmt --all -- --check
    cargo fmt --all --manifest-path fuzz/Cargo.toml -- --check
    cargo clippy --locked --workspace --all-targets -- -D warnings
    cargo clippy --locked --manifest-path fuzz/Cargo.toml -- -D warnings
    RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --no-deps --document-private-items
    typos

# Needs the 1.91 toolchain: `rustup toolchain install 1.91`.

# Check that the code builds with the oldest supported compiler
msrv:
    cargo +1.91 check --locked --workspace --all-targets

# Licences, known vulnerabilities and duplicate versions of dependencies
audit:
    cargo deny check

# Check that every combination of features builds
features:
    cargo hack check --locked --feature-powerset --all-targets

# Miri detects undefined behaviour. Needs a nightly toolchain with miri.

# Run the tests that touch neither disk nor network under Miri
miri:
    cargo +nightly miri test --locked -p model --lib

# Needs a nightly toolchain and cargo-fuzz.

# Feed random bytes to every parser of untrusted input, for this many seconds each
fuzz seconds="10":
    cargo xtask fuzz {{seconds}}

# The model test swaps in loom's locks, which need their own build directory.

# Check a transfer's shared state under every interleaving of two fetchers
loom:
    RUSTFLAGS="--cfg mux_loom" CARGO_TARGET_DIR=target/loom cargo test --locked -p engine --lib --release -- interleavings::

# Needs Linux, Valgrind and gungraun-runner: see docs/performance.md.

# Count the instructions the hot paths execute, e.g. `just bench origin/main` to compare with main
bench base="":
    cargo xtask bench {{base}}

# ---- Our own analysis. Not part of CI: run it before opening a pull request ----

# Every local analysis check
analysis: workflows unused dupes pure deps size kani

# Needs Kani: `cargo install --locked kani-verifier && cargo kani setup`.

# Prove the core's arithmetic and state machines for every input up to a bound
kani:
    cargo kani -p model

# Check the CI workflow files for mistakes GitHub would reject
workflows:
    actionlint

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
    cargo check --locked -p model --no-default-features --target thumbv7em-none-eabihf
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

# Needs Lean and Aeneas: see proofs/README.md. Not part of `just all`,
# because the tools take several gigabytes. Run it whenever a proven module
# of the core changes.

# Translate the core's proven modules to Lean again and check every proof
proofs:
    cargo xtask proofs

# Slow: every mutant rebuilds and reruns the tests. A mutant that survives is
# either a missing test or code that nothing needs. Not part of `just all`;
# run it before a release. Needs cargo-mutants.

# Change the core's code in small ways and check that some test notices
mutants:
    cargo mutants --package model

# Time a loopback transfer, e.g. `just transfer 4096 2` for 4 GiB from two seeders
transfer size_mb="1024" seeders="1" runs="3" files="1":
    MUX_BENCH_SIZE_MB={{size_mb}} MUX_BENCH_SEEDERS={{seeders}} MUX_BENCH_RUNS={{runs}} MUX_BENCH_FILES={{files}} cargo bench --locked --package benches --bench transfer

# Time a download from a web address, e.g. `just fetch http://127.0.0.1:8080/big.bin 1` over one stream
fetch url streams="4" runs="3":
    MUX_BENCH_URL={{url}} MUX_BENCH_STREAMS={{streams}} MUX_BENCH_RUNS={{runs}} cargo bench --locked --package benches --bench web

# Run mux from source, e.g. `just run seed ./model.bin`
run *args:
    cargo run --release -p cli -- {{args}}

# Install mux into ~/.cargo/bin
install:
    cargo install --path crates/cli --locked

# Build with Nix and run the flake checks
nix:
    nix build
    nix flake check
