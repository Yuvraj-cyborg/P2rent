-- Facts used by every proof file: what the hand-written models in
-- FunsExternal.lean do, and what `to_usize` returns.
module
public import Aeneas
public import Model.Funs
@[expose] public section
open Aeneas Aeneas.Std Result

namespace model

/-- `usize::try_from(u64)` gives back the same number when it fits. -/
@[step]
theorem Usize.Insts.CoreConvertTryFromU64TryFromIntError.try_from.spec (i : U64) :
    Usize.Insts.CoreConvertTryFromU64TryFromIntError.try_from i
    ⦃ (r : core.result.Result Usize core.num.error.TryFromIntError) =>
      (i.val ≤ Usize.max → ∃ x : Usize, r = .Ok x ∧ x.val = i.val) ∧
      (Usize.max < i.val → r = .Err ()) ⦄ := by
  unfold Usize.Insts.CoreConvertTryFromU64TryFromIntError.try_from
    core.num.tryFromUScalar
  split <;> rename_i hb <;> simp only [WP.spec_ok]
  · refine ⟨fun _ => ⟨_, rfl, ?_⟩, fun habsurd => by scalar_tac⟩
    have hcast :=
      (UScalar.cast_inBounds_spec (src_ty := .U64) (tgt_ty := .Usize) i
        (by scalar_tac))
    simp only [lift, WP.spec_ok] at hcast
    exact hcast
  · refine ⟨fun habsurd => by scalar_tac, fun _ => trivial⟩

/-- `u32::try_from(u64)` gives back the same number when it fits. -/
@[step]
theorem U32.Insts.CoreConvertTryFromU64TryFromIntError.try_from.spec (i : U64) :
    U32.Insts.CoreConvertTryFromU64TryFromIntError.try_from i
    ⦃ (r : core.result.Result U32 core.num.error.TryFromIntError) =>
      (i.val ≤ U32.max → ∃ x : U32, r = .Ok x ∧ x.val = i.val) ∧
      (U32.max < i.val → r = .Err ()) ⦄ := by
  unfold U32.Insts.CoreConvertTryFromU64TryFromIntError.try_from
    core.num.tryFromUScalar
  split <;> rename_i hb <;> simp only [WP.spec_ok]
  · refine ⟨fun _ => ⟨_, rfl, ?_⟩, fun habsurd => by scalar_tac⟩
    have hcast :=
      (UScalar.cast_inBounds_spec (src_ty := .U64) (tgt_ty := .U32) i
        (by scalar_tac))
    simp only [lift, WP.spec_ok] at hcast
    exact hcast
  · refine ⟨fun habsurd => by scalar_tac, fun _ => trivial⟩

/-- `to_usize` keeps a number that fits, and clamps one that does not to
the largest `usize`. It never fails. -/
@[step]
theorem artifact.to_usize.spec (n : U64) :
    artifact.to_usize n
    ⦃ r => (n.val ≤ Usize.max → r.val = n.val) ∧
           (Usize.max < n.val → r.val = Usize.max) ⦄ := by
  unfold artifact.to_usize
  step as ⟨ r, hfits, hover ⟩
  by_cases h : n.val ≤ Usize.max
  · obtain ⟨x, hx, hval⟩ := hfits h
    simp [hx, hval]
    omega
  · have hr := hover (by omega)
    simp [hr]
    omega

/-- A `usize` is never wider than a `u64`. -/
theorem usize_max_le_u64_max : Usize.max ≤ U64.max := by
  have := Usize.bounds_eq
  scalar_tac

end model
