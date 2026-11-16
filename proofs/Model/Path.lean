-- What is proven about `model::artifact::path`.
--
-- A manifest comes from another machine, and its paths decide where files
-- are written. These are the checks every path has to pass.
--
--   * `precedes` is the order of byte strings. Paths that are listed in that
--     order are all different, so a manifest that passes has no two entries
--     with one path.
--   * A name that `is_safe_name` accepts is not empty, not `.` or `..`, and
--     holds no `/`, `\`, `:` or NUL. Such a name means one level down, inside
--     the directory it is in, on every platform. A path made of such names
--     cannot lead out of the directory it starts from.
--   * No check can fail, whatever bytes it is given: none reads past the end
--     of a name or overflows.
--
-- The checks that split a whole path into names, `is_safe_path` and
-- `is_safe_target`, are translated here too. They are covered by bounded
-- proofs and fuzzing, not yet by theorems.
module
public import Aeneas
public import Model.Basics
@[expose] public section
open Aeneas Aeneas.Std Result

namespace model

open artifact.path

/-! ## The order of paths -/

/-- `a` comes before `b` in the order of byte strings: at the first place
where they differ `a` has the smaller byte, or `a` ends first. -/
def Before : List U8 → List U8 → Prop
  | [], [] => False
  | [], _ :: _ => True
  | _ :: _, [] => False
  | x :: xs, y :: ys => x.val < y.val ∨ (x = y ∧ Before xs ys)

/-- Nothing comes before itself. -/
theorem Before.irrefl (a : List U8) : ¬ Before a a := by
  induction a with
  | nil => simp [Before]
  | cons x xs ih => simp [Before, ih]

theorem Before.trans {a b c : List U8} (hab : Before a b) (hbc : Before b c) :
    Before a c := by
  induction a generalizing b c with
  | nil =>
    cases b with
    | nil => simp [Before] at hab
    | cons y ys =>
      cases c with
      | nil => simp [Before] at hbc
      | cons z zs => simp [Before]
  | cons x xs ih =>
    cases b with
    | nil => simp [Before] at hab
    | cons y ys =>
      cases c with
      | nil => simp [Before] at hbc
      | cons z zs =>
        simp only [Before] at hab hbc ⊢
        rcases hab with hxy | ⟨ hxy, hrest ⟩
        · rcases hbc with hyz | ⟨ hyz, _ ⟩
          · left
            omega
          · left
            rw [← hyz]
            exact hxy
        · rw [hxy]
          rcases hbc with hyz | ⟨ hyz, hrest' ⟩
          · left
            exact hyz
          · right
            exact ⟨ hyz, ih hrest hrest' ⟩

/-- What comes before something is not that thing. -/
theorem Before.ne {a b : List U8} (h : Before a b) : a ≠ b := by
  intro heq
  rw [heq] at h
  exact Before.irrefl b h

/-- Paths listed in order are all different: if each of `count` paths comes
before the next, no two of them are the same. -/
theorem paths_in_order_are_distinct (path : Nat → List U8) (count : Nat)
    (hordered : ∀ i, i + 1 < count → Before (path i) (path (i + 1))) :
    ∀ i j, i < j → j < count → path i ≠ path j := by
  intro i j hij hj
  have hbefore : ∀ d, i + d + 1 < count → Before (path i) (path (i + d + 1)) := by
    intro d
    induction d with
    | zero =>
      intro h
      exact hordered i h
    | succ e ih =>
      intro h
      exact Before.trans (ih (by omega)) (hordered (i + e + 1) (by omega))
  have hreach := hbefore (j - i - 1) (by omega)
  have hj' : i + (j - i - 1) + 1 = j := by omega
  rw [hj'] at hreach
  exact Before.ne hreach

/-- Reading a list from a place inside it gives the element there, then the
rest. -/
theorem drop_at (xs : List U8) (i : Nat) (x : U8) (hi : i < xs.length) (hx : x = xs[i]) :
    xs.drop i = x :: xs.drop (i + 1) := by
  rw [hx]
  exact List.drop_eq_getElem_cons hi

theorem artifact.path.precedes_loop.spec (a b : Slice U8) (start : Usize)
    (hinv : start.val ≤ a.val.length ∧ start.val ≤ b.val.length ∧
      (Before a.val b.val ↔ Before (a.val.drop start.val) (b.val.drop start.val))) :
    artifact.path.precedes_loop a b start ⦃ r => r = true ↔ Before a.val b.val ⦄ := by
  unfold artifact.path.precedes_loop
  apply loop.spec_decr_nat
    (measure := fun (i : Usize) => a.val.length - i.val)
    (inv := fun (i : Usize) => i.val ≤ a.val.length ∧ i.val ≤ b.val.length ∧
      (Before a.val b.val ↔ Before (a.val.drop i.val) (b.val.drop i.val)))
  · intro i hstate
    obtain ⟨ hia, hib, hsame ⟩ := hstate
    simp only [artifact.path.precedes_loop.body]
    split
    · rename_i hlta
      have hina : i.val < a.val.length := by scalar_tac
      split
      · rename_i hltb
        have hinb : i.val < b.val.length := by scalar_tac
        step as ⟨ x, hx ⟩
        step as ⟨ y, hy ⟩
        have hdropa := drop_at a.val i.val x hina hx
        have hdropb := drop_at b.val i.val y hinb hy
        split
        · rename_i hdiffer
          have hne : x ≠ y := by simpa using hdiffer
          simp only [WP.spec_ok]
          rw [hsame, hdropa, hdropb]
          simp only [Before, decide_eq_true_eq]
          constructor
          · intro hlt
            left
            scalar_tac
          · intro h
            rcases h with hlt | ⟨ heq, _ ⟩
            · scalar_tac
            · exact absurd heq hne
        · rename_i hsamebyte
          have heq : x = y := by
            rw [UScalar.eq_equiv]
            simpa using hsamebyte
          step as ⟨ next, hnext ⟩
          refine ⟨ by scalar_tac, by scalar_tac, ?_, by scalar_tac ⟩
          rw [hsame, hdropa, hdropb, hnext, heq]
          simp [Before]
      · rename_i hgeb
        -- `b` has ended and `a` has not, so `a` does not come first.
        have hend : i.val = b.val.length := by scalar_tac
        have hbnil : b.val.drop i.val = [] := by
          rw [hend]
          simp
        simp only [WP.spec_ok]
        rw [hsame, List.drop_eq_getElem_cons hina, hbnil]
        simp only [Before, decide_eq_true_eq, iff_false]
        scalar_tac
    · rename_i hgea
      -- `a` has ended. It comes first exactly if `b` has not.
      have hend : i.val = a.val.length := by scalar_tac
      have hanil : a.val.drop i.val = [] := by
        rw [hend]
        simp
      simp only [WP.spec_ok]
      rw [hsame, hanil]
      by_cases hmore : i.val < b.val.length
      · rw [List.drop_eq_getElem_cons hmore]
        simp only [Before, decide_eq_true_eq, iff_true]
        scalar_tac
      · rw [List.drop_eq_nil_of_le (by omega)]
        simp only [Before, decide_eq_true_eq, iff_false]
        scalar_tac
  · exact hinv

/-- `precedes` says whether one byte string comes before another, and
cannot fail. -/
@[step]
theorem artifact.path.precedes.spec (a b : Slice U8) :
    artifact.path.precedes a b ⦃ r => r = true ↔ Before a.val b.val ⦄ := by
  unfold artifact.path.precedes
  exact artifact.path.precedes_loop.spec a b 0#usize ⟨ by scalar_tac, by scalar_tac, by simp ⟩

/-! ## Names -/

/-- A byte that can never be part of a name: `/`, `\`, `:` or NUL. -/
def Separator (b : U8) : Prop :=
  b = 47#u8 ∨ b = 92#u8 ∨ b = 58#u8 ∨ b = 0#u8

/-- A name that means one level down, inside the directory it is in, on
every platform: it is not empty, not too long, not `.` or `..`, and holds no
separator. -/
def SafeName (n : List U8) : Prop :=
  n ≠ [] ∧ n.length ≤ 246 ∧ n ≠ [46#u8] ∧ n ≠ [46#u8, 46#u8] ∧ ∀ x ∈ n, ¬ Separator x

@[step]
theorem artifact.path.is_separator.spec (b : U8) :
    artifact.path.is_separator b ⦃ r => r = true ↔ Separator b ⦄ := by
  unfold artifact.path.is_separator
  split
  · simp_all [Separator]
  · split
    · simp_all [Separator]
    · split
      · simp_all [Separator]
      · simp_all [Separator]

/-- `is_current` recognises `.`, and cannot fail. -/
@[step]
theorem artifact.path.is_current.spec (n : Slice U8) :
    artifact.path.is_current n ⦃ r => n.val = [46#u8] → r = true ⦄ := by
  simp only [artifact.path.is_current]
  split
  · rename_i hlen
    simp only [WP.spec_ok]
    intro hdot
    have hone : n.val.length = 1 := by
      rw [hdot]
      rfl
    scalar_tac
  · rename_i hlen
    have hone : n.val.length = 1 := by scalar_tac
    step as ⟨ first, hfirst ⟩
    intro hdot
    simp [hdot] at hfirst
    simp [hfirst]

/-- `is_parent` recognises `..`, and cannot fail. -/
@[step]
theorem artifact.path.is_parent.spec (n : Slice U8) :
    artifact.path.is_parent n ⦃ r => n.val = [46#u8, 46#u8] → r = true ⦄ := by
  simp only [artifact.path.is_parent]
  split
  · rename_i hlen
    simp only [WP.spec_ok]
    intro hdots
    have htwo : n.val.length = 2 := by
      rw [hdots]
      rfl
    scalar_tac
  · rename_i hlen
    have htwo : n.val.length = 2 := by scalar_tac
    step as ⟨ first, hfirst ⟩
    split
    · rename_i hnot
      simp only [WP.spec_ok]
      intro hdots
      simp [hdots] at hfirst
      simp [hfirst] at hnot
    · step as ⟨ second, hsecond ⟩
      intro hdots
      simp [hdots] at hsecond
      simp [hsecond]

/-- `lower` cannot fail: a capital letter plus 32 is still a byte. -/
@[step]
theorem artifact.path.lower.spec (b : U8) : artifact.path.lower b ⦃ _ => True ⦄ := by
  unfold artifact.path.lower
  split
  · simp
  · split
    · simp
    · step as ⟨ r, hr ⟩

theorem artifact.path.is_part_name_loop.spec (n : Slice U8) (start at_ : Usize)
    (hinv : start.val + 9 = n.val.length ∧ at_.val ≤ 9) :
    artifact.path.is_part_name_loop n start at_ ⦃ _ => True ⦄ := by
  unfold artifact.path.is_part_name_loop
  apply loop.spec_decr_nat
    (measure := fun (i : Usize) => 9 - i.val)
    (inv := fun (i : Usize) => start.val + 9 = n.val.length ∧ i.val ≤ 9)
  · intro i hstate
    obtain ⟨ hstart, hle ⟩ := hstate
    simp only [artifact.path.is_part_name_loop.body]
    step as ⟨ suffix, hsuffix ⟩
    have hnine : suffix.val.length = 9 := by
      rw [hsuffix]
      scalar_tac
    split
    · rename_i hlt
      have hin : i.val < 9 := by scalar_tac
      step as ⟨ place, hplace ⟩
      step as ⟨ byte, hbyte ⟩
      step as ⟨ small ⟩
      step as ⟨ wanted, hwanted ⟩
      split
      · simp only [WP.spec_ok]
      · step as ⟨ next, hnext ⟩
        exact ⟨ hstart, by scalar_tac, by scalar_tac ⟩
    · simp only [WP.spec_ok]
  · exact hinv

/-- `is_part_name` cannot fail, whatever the name. -/
@[step]
theorem artifact.path.is_part_name.spec (n : Slice U8) :
    artifact.path.is_part_name n ⦃ _ => True ⦄ := by
  simp only [artifact.path.is_part_name]
  step as ⟨ suffix, hsuffix ⟩
  have hnine : suffix.val.length = 9 := by
    rw [hsuffix]
    scalar_tac
  split
  · simp only [WP.spec_ok]
  · rename_i hlong
    step as ⟨ again, hagain ⟩
    have hnine' : again.val.length = 9 := by
      rw [hagain]
      scalar_tac
    step as ⟨ start, hstart ⟩
    exact artifact.path.is_part_name_loop.spec n start 0#usize
      ⟨ by scalar_tac, by scalar_tac ⟩

theorem artifact.path.is_safe_name_loop.spec (n : Slice U8) (start : Usize)
    (hinv : start.val ≤ n.val.length ∧
      ∀ k x, k < start.val → n.val[k]? = some x → ¬ Separator x) :
    artifact.path.is_safe_name_loop n start
    ⦃ r => r = true → ∀ x ∈ n.val, ¬ Separator x ⦄ := by
  unfold artifact.path.is_safe_name_loop
  apply loop.spec_decr_nat
    (measure := fun (i : Usize) => n.val.length - i.val)
    (inv := fun (i : Usize) => i.val ≤ n.val.length ∧
      ∀ k x, k < i.val → n.val[k]? = some x → ¬ Separator x)
  · intro i hstate
    obtain ⟨ hle, hclean ⟩ := hstate
    simp only [artifact.path.is_safe_name_loop.body]
    split
    · rename_i hlt
      have hin : i.val < n.val.length := by scalar_tac
      step as ⟨ byte, hbyte ⟩
      step as ⟨ bad, hbad ⟩
      split
      · simp only [WP.spec_ok]
        intro h
        cases h
      · rename_i hfine
        step as ⟨ next, hnext ⟩
        refine ⟨ by scalar_tac, ?_, by scalar_tac ⟩
        intro k x hk hx
        by_cases hprev : k < i.val
        · exact hclean k x hprev hx
        · have hki : k = i.val := by scalar_tac
          subst hki
          rw [List.getElem?_eq_getElem hin] at hx
          have hxb : x = byte := by
            rw [hbyte]
            exact (Option.some.inj hx).symm
          rw [hxb]
          intro hseparator
          exact hfine (hbad.mpr hseparator)
    · rename_i hend
      have hall : i.val = n.val.length := by scalar_tac
      step as ⟨ part ⟩
      intro _ x hx
      obtain ⟨ k, hk ⟩ := List.mem_iff_getElem?.mp hx
      obtain ⟨ hklt, _ ⟩ := List.getElem?_eq_some_iff.mp hk
      exact hclean k x (by omega) hk
  · exact hinv

/-- A name that `is_safe_name` accepts is a safe name, and the check cannot
fail. -/
@[step]
theorem artifact.path.is_safe_name.spec (n : Slice U8) :
    artifact.path.is_safe_name n ⦃ r => r = true → SafeName n.val ⦄ := by
  simp only [artifact.path.is_safe_name]
  step as ⟨ empty, hempty ⟩
  split
  · simp only [WP.spec_ok]
    intro h
    cases h
  · rename_i hnotempty
    split
    · simp only [WP.spec_ok]
      intro h
      cases h
    · rename_i hshort
      step as ⟨ current, hcurrent ⟩
      split
      · simp only [WP.spec_ok]
        intro h
        cases h
      · rename_i hnotcurrent
        step as ⟨ parent, hparent ⟩
        split
        · simp only [WP.spec_ok]
          intro h
          cases h
        · rename_i hnotparent
          apply WP.spec_mono (artifact.path.is_safe_name_loop.spec n 0#usize
            ⟨ by scalar_tac, by intro k x hk; simp at hk ⟩)
          intro r hclean haccepted
          have hlength : n.val.length ≤ 246 := by
            simp only [artifact.path.MAX_NAME_BYTES] at hshort
            scalar_tac
          refine ⟨ ?_, hlength, fun hdot => hnotcurrent (hcurrent hdot),
            fun hdots => hnotparent (hparent hdots), hclean haccepted ⟩
          intro hnil
          apply hnotempty
          rw [hempty]
          simp [Slice.length, hnil]

end model
