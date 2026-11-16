# Proofs

The logic in `model` that decides what happens to files is translated
from Rust to Lean 4 and proven here. A proof holds for every input, where a
test covers only the inputs someone tried.

## What is proven

Five modules of `crates/model` are translated:

| Module | Holds |
| --- | --- |
| `artifact::bitfield` | The set of chunks a node holds. |
| `artifact::layout` | Which file, offset and length each chunk has. |
| `artifact::path` | Which names and paths a manifest may use. Proven so far: the check on a single name, and the order that keeps paths distinct. |
| `assembly` | Which files are whole, and so may take their final names. |
| `schedule` | Which chunk to fetch next, and what state each is in. |

The theorems are in `Model/`, one file per module, each beginning with a
plain statement of what it proves.

## How it works

```
Rust  --charon-->  LLBC  --aeneas-->  Lean  <--  theorems written by hand
```

1. [Charon](https://github.com/AeneasVerif/charon) reads the Rust compiler's
   view of the chosen modules.
2. [Aeneas](https://github.com/AeneasVerif/aeneas) turns that into Lean
   functions: `Model/Types.lean` and `Model/Funs.lean`. These two
   files are generated. Do not edit them.
3. The theorems are about those generated functions, so they are about the
   code that ships and not about a copy of it.

`cargo xtask proofs` (or `just proofs`) does the translation again and then
checks every proof. If the two generated files change, the Rust code changed:
commit them with it.

## What is trusted

A proof is only as good as what it rests on. Here that is:

- Lean's kernel, and the Rust compiler.
- Charon and Aeneas: that the Lean they produce means the same as the Rust.
- `Model/FunsExternal.lean`: models, written by hand, of the three
  standard-library functions the code calls that Aeneas does not define
  (`u32::try_from(u64)`, `usize::try_from(u64)` and `u8::count_ones`).
  `cargo xtask proofs` fails if the code starts calling one that has no
  model, so this list cannot grow unnoticed.

Nothing here says anything about disks, networks or time. Those are covered
by tests, fuzzing and simulation.

## Writing code that can be proven

The translator accepts a plain subset of Rust. In the five modules above:

- loops and indexing, not iterators and closures;
- no strings, hashing, I/O or `unsafe`;
- nothing but the logic. An encoding for the wire belongs elsewhere, as
  `artifact/wire.rs` does for the bitfield.

A module joins the list by being added to `PROVEN_MODULES` in
`xtask/src/main.rs`.

## Setting up

The tools take several gigabytes, so this check is not part of `just all`.

1. Install [elan](https://github.com/leanprover/elan), the Lean version
   manager. It reads `lean-toolchain` and fetches the right Lean.
2. Download the Aeneas release named by `AENEAS_RELEASE` in
   `xtask/src/main.rs` for your platform, unpack it, and put the folder on
   your path. It contains both `aeneas` and the matching `charon`.
3. In this directory, run `lake exe cache get` once. It downloads the
   compiled maths library instead of building it, which would take hours.
4. Run `just proofs` from the repository root.

To move to a newer Aeneas, change `AENEAS_RELEASE`, the `rev` in
`lakefile.toml` and `lean-toolchain` together. The last two come from the
Aeneas repository at that release: `backends/lean/lean-toolchain`.
