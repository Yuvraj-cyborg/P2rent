# Proofs

The logic in `model` that decides what happens to files is translated
from Rust to Lean 4 and proven here. A proof holds for every input, where a
test covers only the inputs someone tried.

## What is proven

Seven modules of `crates/model` are translated:

| Module | Holds |
| --- | --- |
| `artifact::bitfield` | The set of chunks a node holds: setting, clearing and reading a bit, counting those that are set, and checking a set that came from another machine. |
| `artifact::layout` | Which file, offset and length each chunk has. |
| `artifact::path` | Which names, paths and link targets a manifest may use: exactly those that are accepted, and that none of them leads out of the destination or is the name of a part file. |
| `assembly` | Which chunks are in place, which files are whole and so may take their final names, and whether everything is there. |
| `mirror::step` | What a sync does about one path: add, replace, set the permissions and time, keep or remove. |
| `rolling` | The hash of a window of bytes that can be moved one byte on, for finding chunks that have moved inside a file. |
| `schedule` | Which chunk to fetch next (the first that can be had, going round from where the search starts), what state each is in, and who is fetching it. |

The theorems are in `Model/`, one file per module, each beginning with a
plain statement of what it proves.

## Are the statements right?

A proof shows that the code does what a statement says. It does not show
that the statement says what was meant. A check of names could be proven
"never to accept an unsafe name" and reject every name there is. Three
things are done about that.

**Statements say what comes back, for every input.** "Exactly when", not
"only if", and in words defined apart from the code: a bit is set, a path
is made of safe names, a count is the number of chunks not in place.

**A condition is one that something meets.** Where a statement holds only
for a bitfield that is tidy, or an assembly that is well formed, another
proof shows that the functions that build such a thing give one, and that
those that change it keep it one.

**Deliberate bugs are put in, and the proofs have to notice.** Each small
change that `cargo mutants` can make to the seven modules is made in turn:
an operator swapped, a function made to return a constant. The Lean is
generated again and every proof checked. A change that every proof
survives shows something the statements do not say.

This was first done on 18 November 2026
([#91](https://github.com/yuvichh/mux/issues/91)):

| | Changes | Noticed | Not noticed |
| --- | ---: | ---: | ---: |
| The proofs as they were | 348 | 293 | 55 |
| Those 55, against the proofs as they are | 55 | 52 | 3 |
| All 348 again, against the proofs as they are | 348 | 345 | 3 |

What the 55 showed:

- Ten functions had no statement at all, among them the count of the bits
  that are set, the check of a bitfield from another machine, and whether
  an assembly is complete. Forty-two of the 55 were changes to those.
- Behind those was a fault. A bitfield can have bits set past its end, in
  the unused part of its last byte, and the count counted them. With
  three of four chunks in place and one such bit set, as many bits are
  set as there are chunks, and an assembly said it was complete with a
  chunk missing. An assembly now takes only a bitfield that the check
  accepts, and it is proven that `new`, `full`, `set` and `clear` never
  make one with such a bit.
- Two functions were proven only not to fail: the one that makes a letter
  small and the one that knows a part file's name. So nothing proved that
  a name the checks accept is not the name of a part file. With the `!`
  taken away from "and it is not a part file's name", every proof still
  passed.
- The checks of names and link targets were proven in one direction. A
  name of exactly the greatest length allowed could be refused, and a
  link target likewise, and every proof still passed.

A change counts as noticed when any proof fails, and that may be a lemma
about a loop which is stronger than the statement about the function. So
the statements were read as well, for the two shapes that hide a weak one:
one direction only, and "cannot fail" with nothing about the result.
Reading found what the bugs did not:

- Every check of a path was proven never to accept what is unsafe, and
  nothing said it accepts what is safe. They are now proven to accept
  exactly the safe names, paths and link targets.
- `claim` was proven to hand out a chunk that can be claimed. It is now
  proven to hand out the first one going round from the cursor, which is
  what a transfer relies on to read chunks in a row.

The last row is the whole run made a second time, to see that nothing the
old proofs noticed had slipped out of the new ones. It had not: the three
left over are the same three.

The three that are still not noticed change nothing, so no statement
could notice them. Each turns `<` into `<=`, or `>` into `>=`, at a place
where that cannot matter:

- in `Bitfield::full`, "there is at least one byte", asked where the
  length has just been found not to be a multiple of eight;
- in `Layout::locate`, "what is left of the file is less than a chunk",
  where being equal gives the same length either way;
- in `precedes`, "this byte is smaller than that one", asked right after
  finding that the two are not the same.

The run is made by hand for now, on a machine with the Lean tools: about
two hours. It is not yet a recipe.

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

The translator accepts a plain subset of Rust. In the seven modules above:

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
