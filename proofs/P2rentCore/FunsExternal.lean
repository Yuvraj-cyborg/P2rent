-- Models of the standard-library functions that the translated code calls
-- and that the Aeneas library does not yet define.
--
-- These are written by hand, so they are trusted rather than proven: each
-- states what the Rust function does. Keep the list short. A function
-- appears here only if `cargo xtask proofs` reports it as missing.
module
public import Aeneas
public import P2rentCore.Types
@[expose] public section
open Aeneas Aeneas.Std Result ControlFlow Error

namespace p2rent_core

/-- `u32::try_from(u64)`: the same number if it fits, an error otherwise. -/
@[rust_fun
  "core::convert::num::{core::convert::TryFrom<u32, u64, core::num::error::TryFromIntError>}::try_from"]
def U32.Insts.CoreConvertTryFromU64TryFromIntError.try_from (i : Std.U64) :
    Result (core.result.Result Std.U32 core.num.error.TryFromIntError) :=
  core.num.tryFromUScalar .U32 i

/-- `usize::try_from(u64)`: the same number if it fits, an error otherwise. -/
@[rust_fun
  "core::convert::num::ptr_try_from_impls::{core::convert::TryFrom<usize, u64, core::num::error::TryFromIntError>}::try_from"]
def Usize.Insts.CoreConvertTryFromU64TryFromIntError.try_from (i : Std.U64) :
    Result (core.result.Result Std.Usize core.num.error.TryFromIntError) :=
  core.num.tryFromUScalar .Usize i

/-- The number of bits set in a bit vector. -/
def popCount {w : Nat} (x : BitVec w) : Nat :=
  (List.range w).countP (fun i => x.getLsbD i)

/-- `u8::count_ones`: the number of bits set in the byte. -/
@[rust_fun "core::num::{u8}::count_ones"]
def core.num.U8.count_ones (x : Std.U8) : Result Std.U32 :=
  ok (U32.ofNat (popCount x.bv) (by
    have : popCount x.bv ≤ 8 := by
      unfold popCount
      exact (List.countP_le_length).trans (by simp)
    scalar_tac))

end p2rent_core
