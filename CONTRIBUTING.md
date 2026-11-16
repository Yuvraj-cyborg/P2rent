# Contributing

p2rent aims to be infrastructure that other software depends on. These rules
keep it small, correct and fast. [docs/scope.md](docs/scope.md) says what the
project is and is not.

Rules 1 and 3 are enforced in review. The others are enforced by CI, and
where CI does not do that yet, the rule names the issue that will add it.

## 1. One issue, one pull request

Work starts as an issue that states the problem, what is in scope, what is
out, what existing code it builds on, when it is done and how it is verified.
The issue template has that shape. One pull request closes the issue and does
nothing else.

## 2. Code is a budget

Every line has to be read, tested, proven and maintained, so each one has to
earn its place.

- Before writing a function, look for one that already does the job.
- Shared logic has one home, `p2rent-core`. A backend is a thin adapter and
  does no scheduling, retrying or verifying of its own.
- A dependency is added only with a stated reason.
- Code that no test notices is deleted.

CI fails on near-duplicate functions (`just dupes`), unused dependencies and
duplicate dependency versions (`just deps`), and dead code (`just lint`). It
also reports the size of the binary and the number of dependencies on every
pull request and fails when either is over the budget set in
`xtask/src/main.rs` (`just size`). A scheduled mutation run will list code
that no test notices
([#11](https://github.com/Yuvraj-cyborg/P2rent/issues/11)).

## 3. Foundations before features

A feature is not built on a design that is known to be wrong. A bug is fixed
at its cause, starting with a test that reproduces it, never with a special
case around it.

## 4. Tests are Rust

- Tests run under `cargo`, and network tests start real servers from the
  test code.
- No shell or Python scripts. A development task that needs logic is written
  as a Rust `xtask` binary. The two scripts left in the repository are
  removed in [#9](https://github.com/Yuvraj-cyborg/P2rent/issues/9).

## 5. Static analysis is required

Every CI job runs a `just` recipe, so `just ci` runs the same checks
locally:

| Recipe | Checks |
| --- | --- |
| `just lint` | Formatting, `clippy` with its pedantic lints, and the documentation build, all with warnings as errors. Every public item is documented. |
| `just deps` | Licences, known vulnerabilities, duplicate versions and unused dependencies. |
| `just spelling` | Spelling in code and documentation. |
| `just dupes` | Functions that are near copies of each other. |
| `just features` | Every combination of features builds. |
| `just miri` | The tests that touch neither disk nor network, run under Miri to detect undefined behaviour. |
| `just size` | Binary size and dependency count against their budgets. |

CI also checks that the code builds with the minimum supported Rust version
and runs the tests on Linux, macOS and Windows.

Library code may not panic on bad input: `unwrap`, `expect` and `panic!` are
errors outside tests. A lint that produces noise without value is switched
off in `Cargo.toml` with a written reason, never with an `allow` in the
code.

## 6. Verification comes in tiers

Each issue says which tiers apply to it.

| Tier | Tool | Used for |
| --- | --- | --- |
| Proven | Lean 4, through Aeneas | The logic that decides what happens to files: chunk layout, scheduling, sync planning. |
| Bounded | Kani | Arithmetic that must never overflow, panic or leave its bounds. |
| Model-checked | `loom` | Shared state under every interleaving. |
| Empirical | Property tests, fuzzing, a simulated network, mutation testing | Parsers of untrusted bytes, and anything that does I/O. |
| Runtime | Hash checks | Every chunk before it is kept, and every file before it is renamed into place. |

Decision logic lives in `p2rent-core` as safe, synchronous Rust, so that it
stays provable. The tiers are set up in
[#11](https://github.com/Yuvraj-cyborg/P2rent/issues/11) and
[#12](https://github.com/Yuvraj-cyborg/P2rent/issues/12).

## 7. Safe by default, with a fenced fast path

`unsafe`, SIMD intrinsics and assembly are allowed in one crate,
`p2rent-accel`. Every other crate forbids `unsafe`. In that crate:

- every fast function has a safe reference version with the same signature;
- a comparison test runs both on random inputs, for every CPU feature level
  the fast version supports;
- a benchmark shows the gain;
- the fast version is chosen at run time, and the safe one stays as the
  fallback.

The fence and the comparison tests are set up in
[#13](https://github.com/Yuvraj-cyborg/P2rent/issues/13).

## 8. Measure before optimising

No optimisation is merged without figures.
[docs/performance.md](docs/performance.md) records what was measured, what
helped and what did not, so the same ground is not covered twice. One change
per pull request. CI will compare every pull request with `main` and fail on
a regression ([#13](https://github.com/Yuvraj-cyborg/P2rent/issues/13)).

## 9. Easy to guess

A command works out what to do from the places it is given. There is no
configuration file, every error says what to do next, and `--json` gives
programs the same information people get. The commands arrive in
[#21](https://github.com/Yuvraj-cyborg/P2rent/issues/21), with tests that fix
their output.

## 10. Library first

The CLI is a thin layer. Every decision is made in the library, and every
kind of place is its own crate behind a feature, so that other programs can
embed exactly what they need.
[#10](https://github.com/Yuvraj-cyborg/P2rent/issues/10) splits the code that
way, and CI will fail if the core gains a dependency on disk or network.

## Commits

Commit messages and pull request titles use a prefix that says what kind of
change it is, with the area in brackets where it helps: `feat(http): …`,
`fix: …`, `perf(net): …`, `docs: …`, `build: …`, `bench: …`.
