# Contributing

mux aims to be infrastructure that other software depends on. These rules
keep it small, correct and fast. [docs/scope.md](docs/scope.md) says what the
project is and is not.

Rules 1 and 3 are enforced in review. The others are checked by a command,
either in CI or locally before a pull request is opened, and each rule names
its command. Run `just all` before opening a pull request: it runs
everything CI runs and everything that is checked only locally.

## 1. One issue, one pull request

Work starts as an issue that states the problem, what is in scope, what is
out, what existing code it builds on, when it is done and how it is verified.
The issue template has that shape. One pull request closes the issue and does
nothing else.

## 2. Code is a budget

Every line has to be read, tested, proven and maintained, so each one has to
earn its place.

- Before writing a function, look for one that already does the job.
- Shared logic has one home, `model`. A backend is a thin adapter and
  does no scheduling, retrying or verifying of its own.
- A dependency is added only with a stated reason.
- Code that no test notices is deleted.

`just analysis` checks this locally:

- `just dupes` fails on functions that are near copies of each other;
- `just unused` fails on dependencies that are declared but never used;
- `just deps` fails if the committed list of crates that ship in the released
  binaries, `xtask/shipped-crates.txt`, is out of date. Adding a dependency
  therefore shows its name in the diff;
- `just size` reports the size of the release binary and fails if it is over
  the budget set in `xtask/src/main.rs`.

Dead code is an error in `just lint`. `just mutants`, run before a release,
changes the core's code in small ways and lists every change that no test
noticed: each one is a missing test or code that nothing needs.

## 3. Foundations before features

A feature is not built on a design that is known to be wrong. A bug is fixed
at its cause, starting with a test that reproduces it, never with a special
case around it.

## 4. Tests are Rust

- Tests run under `cargo` (with `cargo-nextest` as the runner), and network
  tests start real servers from the test code.
- No shell or Python scripts. A development task that needs logic is written
  as a Rust `xtask` binary and run with `cargo xtask`.

## 5. Static analysis is required

CI runs the checks that any project of this kind runs. `just ci` runs the
same ones locally.

| CI job | Recipe | Checks |
| --- | --- | --- |
| `test` | `just test` | Builds every target and runs the tests, on Linux, macOS and Windows with the stable compiler and on Linux with beta. |
| `msrv` | `just msrv` | The code builds with the oldest supported compiler. |
| `lint` | `just lint` | Formatting, `clippy` with its pedantic lints, the documentation build and spelling, all with warnings as errors. Every public item is documented. |
| `audit` | `just audit` | Licences, known vulnerabilities and duplicate versions of dependencies. |
| `features` | `just features` | Every combination of features builds. |
| `miri` | `just miri` | The tests that touch neither disk nor network, run under Miri to detect undefined behaviour. |
| `fuzz` | `just fuzz` | Every parser of bytes from other machines, fed random input for ten seconds each as a smoke test. |
| `loom` | `just loom` | The download's shared state, under every interleaving of two fetchers. |
| `bench` | `just bench origin/main` | The instructions each hot path executes, compared with the base branch. A rise of more than 5% fails. Needs Valgrind, so it runs on Linux only and is not part of `just ci`. |

The checks that are this project's own analysis run locally, with
`just analysis`:

| Recipe | Checks |
| --- | --- |
| `just workflows` | The CI workflow files are valid. |
| `just unused` | No dependency is declared and never used. |
| `just dupes` | No function is a near copy of another. |
| `just pure` | `model` builds with no standard library and depends only on the crates it is allowed to. |
| `just deps` | The committed list of shipped crates is up to date. |
| `just size` | The release binary is within its size budget. |
| `just kani` | The core's arithmetic and state machines, proven for every input up to a bound. |

Library code may not panic on bad input: `unwrap`, `expect` and `panic!` are
errors outside tests. Both that rule and the one that every public item is
documented are about a library's public surface, so each library sets them
at the top of its `lib.rs`; a new library crate copies those lines. A lint
that produces noise without value is switched off in `Cargo.toml` with a
written reason, never with an `allow` in the code.

## 6. Verification comes in tiers

Each issue says which tiers apply to it.

| Tier | Tool | Run with | Used for |
| --- | --- | --- | --- |
| Proven | Lean 4, through Aeneas | `just proofs` | The logic that decides what happens to files. Today: the bitfield, the chunk layout and the schedule, for inputs of any size. |
| Bounded | Kani | `just kani` | Arithmetic and state machines that must never overflow, panic or leave their bounds, for every input up to a size limit. |
| Model-checked | `loom` | `just loom` | Shared state under every interleaving. |
| Empirical | Property tests | `just test` | Round trips, and behaviour compared with a simple model. |
| | Fuzzing | `just fuzz` | Parsers of untrusted bytes. A long run is `just fuzz 600`. |
| | Mutation testing | `just mutants` | Whether the tests would notice a wrong change. |
| Runtime | Hash checks | always on | Every chunk before it is kept, and every file before it is renamed into place. |

A property is written once, at the strongest tier that can express it. A
bug found by any tier is fixed at its cause, and the input that found it is
kept as a test.

Decision logic lives in `model`, in modules that hold nothing else and
are written plainly: loops and indexing, no iterators, closures or strings.
That is what lets them be translated to Lean and proven.
[proofs/README.md](proofs/README.md) says how, and what the proofs rest on.
`just proofs` needs Lean and Aeneas, which take several gigabytes, so it is
not part of `just all`: run it whenever one of those modules changes.

## 7. Safe by default, with a fenced fast path

`unsafe`, SIMD intrinsics and assembly are allowed in one crate, `accel`.
Every other crate forbids `unsafe`. This is the method FFmpeg uses for its
hand-written assembly, with `checkasm` as the comparison test.

- `accel` is created by the first pull request that needs it, not before.
- Every fast function has a safe reference version with the same signature.
- A comparison test runs both versions on random inputs, for every CPU
  feature level the fast version supports.
- A benchmark must show the gain. The figures go into
  [docs/performance.md](docs/performance.md), including for a change that
  did not help and was dropped.
- The fast version is chosen at run time by CPU detection. The safe version
  stays as the fallback, and is what runs where no fast version exists.
- Miri and the sanitizers run on `accel`.

## 8. Measure before optimising

No optimisation is merged without figures, and one pull request makes one
change. [docs/performance.md](docs/performance.md) says how each figure is
taken and records what was measured, what helped and what did not, so the
same ground is not covered twice.

- Each hot path that runs on one thread has a benchmark that counts the
  instructions it executes. The count is the same on every run, so CI
  compares each pull request with its base branch and fails on a rise of
  more than 5%. A pull request that adds a hot path adds its benchmark.
- A pull request that has to raise a count beyond the limit raises the limit
  for that one benchmark, with the reason beside it.
- A timed transfer over loopback runs every night. It moves with the
  machine, so it is a record and not a gate.

## 9. Easy to guess

A command works out what to do from the places it is given. There is no
configuration file, every error says what to do next, and `--json` gives
programs the same information people get. The commands arrive in
[#21](https://github.com/Yuvraj-cyborg/mux/issues/21), with tests that fix
their output.

## 10. Library first

The CLI is a thin layer. Every decision is made in the library, and every
kind of place is its own crate behind a feature, so that other programs can
embed exactly what they need. `just pure` fails if the core gains a
dependency on a disk, a network or a clock.

## Commits

Commit messages and pull request titles use a prefix that says what kind of
change it is, with the area in brackets where it helps: `feat(http): …`,
`fix: …`, `perf(net): …`, `docs: …`, `build: …`, `bench: …`.
