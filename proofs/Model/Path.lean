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
--     the directory it is in, on every platform.
--   * A path that `is_safe_path` accepts is such names joined by `/`, and
--     nothing else. So it cannot lead out of the directory it starts from.
--   * A link target that `is_safe_target` accepts, followed one name at a
--     time from the directory that holds the link, never climbs above the
--     root.
--   * `parent_len` is the place of the last `/`, so what comes before it is
--     the directory that holds the entry. `depth` is the number of names.
--   * No check can fail, whatever bytes it is given: none reads past the end
--     or overflows.
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

/-- A capital letter as its small letter, and any other byte as itself. -/
def lowerOf (b : U8) : Nat :=
  if 65 ≤ b.val ∧ b.val ≤ 90 then b.val + 32 else b.val

/-- What is added to a file's name while it is being written: `.mux-part`. -/
def partSuffix : List U8 :=
  [46#u8, 109#u8, 117#u8, 120#u8, 45#u8, 112#u8, 97#u8, 114#u8, 116#u8]

/-- A name that ends with the suffix of a part file, in any letter case. A
file of such a name could be taken for the part file of another. -/
def PartName (n : List U8) : Prop :=
  9 ≤ n.length ∧
  ∀ k, k < 9 → lowerOf (n.getD (n.length - 9 + k) 0#u8) = (partSuffix.getD k 0#u8).val

/-- A name that means one level down, inside the directory it is in, on
every platform, and that no part file has: it is not empty, not too long,
not `.` or `..`, holds no separator, and does not end as a part file's name
does. -/
def SafeName (n : List U8) : Prop :=
  n ≠ [] ∧ n.length ≤ 246 ∧ n ≠ [46#u8] ∧ n ≠ [46#u8, 46#u8] ∧
  (∀ x ∈ n, ¬ Separator x) ∧ ¬ PartName n

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

/-- `is_current` says yes exactly for `.`, and cannot fail. -/
@[step]
theorem artifact.path.is_current.spec (n : Slice U8) :
    artifact.path.is_current n ⦃ r => r = true ↔ n.val = [46#u8] ⦄ := by
  simp only [artifact.path.is_current]
  split
  · rename_i hlen
    simp only [WP.spec_ok]
    constructor
    · intro h
      cases h
    · intro hdot
      have hone : n.val.length = 1 := by
        rw [hdot]
        rfl
      scalar_tac
  · rename_i hlen
    have hone : n.val.length = 1 := by scalar_tac
    obtain ⟨ one, hsingle ⟩ := List.length_eq_one_iff.mp hone
    step as ⟨ first, hfirst ⟩
    simp only [hsingle, List.getElem_cons_zero] at hfirst
    rw [hsingle, hfirst]
    simp

/-- `is_parent` says yes exactly for `..`, and cannot fail. -/
@[step]
theorem artifact.path.is_parent.spec (n : Slice U8) :
    artifact.path.is_parent n ⦃ r => r = true ↔ n.val = [46#u8, 46#u8] ⦄ := by
  simp only [artifact.path.is_parent]
  split
  · rename_i hlen
    simp only [WP.spec_ok]
    constructor
    · intro h
      cases h
    · intro hdots
      have htwo : n.val.length = 2 := by
        rw [hdots]
        rfl
      scalar_tac
  · rename_i hlen
    have htwo : n.val.length = 2 := by scalar_tac
    obtain ⟨ first, second, hpair ⟩ := List.length_eq_two.mp htwo
    step as ⟨ one, hone ⟩
    simp only [hpair, List.getElem_cons_zero] at hone
    split
    · rename_i hnot
      simp only [WP.spec_ok]
      constructor
      · intro h
        cases h
      · intro hdots
        rw [hpair] at hdots
        simp only [List.cons.injEq, and_true] at hdots
        rw [hone, hdots.1] at hnot
        simp at hnot
    · rename_i hdot
      have hfirst : first = 46#u8 := by
        rw [← hone, UScalar.eq_equiv]
        simpa using hdot
      step as ⟨ two, htwo' ⟩
      simp only [hpair, List.getElem_cons_succ, List.getElem_cons_zero] at htwo'
      rw [hpair, htwo', hfirst]
      simp

/-- `lower` gives a capital letter as its small letter and any other byte
as itself. It cannot fail: a capital letter plus 32 is still a byte. -/
@[step]
theorem artifact.path.lower.spec (b : U8) :
    artifact.path.lower b ⦃ r => r.val = lowerOf b ⦄ := by
  unfold artifact.path.lower
  split
  · rename_i hbelow
    have : b.val < 65 := by scalar_tac
    simp only [WP.spec_ok, lowerOf]
    split <;> omega
  · rename_i hnotbelow
    split
    · rename_i habove
      have : 90 < b.val := by scalar_tac
      simp only [WP.spec_ok, lowerOf]
      split <;> omega
    · rename_i hnotabove
      have hcapital : 65 ≤ b.val ∧ b.val ≤ 90 := by scalar_tac
      step as ⟨ r, hr ⟩
      simp only [lowerOf, hcapital, and_self, ↓reduceIte]
      scalar_tac

/-- The suffix as the code holds it is the suffix as it is written here. -/
theorem part_suffix_val : artifact.path.PART_SUFFIX.val = partSuffix := by
  unfold artifact.path.PART_SUFFIX
  rfl

theorem artifact.path.is_part_name_loop.spec (n : Slice U8) (start at_ : Usize)
    (hinv : start.val + 9 = n.val.length ∧ at_.val ≤ 9 ∧
      ∀ k, k < at_.val →
        lowerOf (n.val.getD (start.val + k) 0#u8) = (partSuffix.getD k 0#u8).val) :
    artifact.path.is_part_name_loop n start at_ ⦃ r => r = true ↔ PartName n.val ⦄ := by
  unfold artifact.path.is_part_name_loop
  apply loop.spec_decr_nat
    (measure := fun (i : Usize) => 9 - i.val)
    (inv := fun (i : Usize) => start.val + 9 = n.val.length ∧ i.val ≤ 9 ∧
      ∀ k, k < i.val →
        lowerOf (n.val.getD (start.val + k) 0#u8) = (partSuffix.getD k 0#u8).val)
  · intro i hstate
    obtain ⟨ hstart, hle, hmatch ⟩ := hstate
    have hfrom : n.val.length - 9 = start.val := by omega
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
      step as ⟨ small, hsmall ⟩
      step as ⟨ wanted, hwanted ⟩
      have hplacelt : place.val < n.val.length := by scalar_tac
      have hhere : n.val.getD (start.val + i.val) 0#u8 = byte := by
        rw [← hplace]
        simp [List.getD_eq_getElem?_getD, hplacelt, hbyte]
      have hthere : partSuffix.getD i.val 0#u8 = wanted := by
        rw [← part_suffix_val]
        have hilt : i.val < artifact.path.PART_SUFFIX.val.length := by
          rw [part_suffix_val]
          exact hin
        rw [hwanted, List.getD_eq_getElem?_getD, List.getElem?_eq_getElem hilt]
        rfl
      split
      · rename_i hdiffer
        -- This byte is not the suffix's, so the name does not end with it.
        have hne : small.val ≠ wanted.val := by scalar_tac
        simp only [WP.spec_ok]
        constructor
        · intro h
          cases h
        · intro hpart
          have hhit := hpart.2 i.val hin
          rw [hfrom, hhere, hthere, ← hsmall] at hhit
          exact absurd hhit hne
      · rename_i hsame
        have heq : small.val = wanted.val := by scalar_tac
        step as ⟨ next, hnext ⟩
        refine ⟨ hstart, by scalar_tac, ?_, by scalar_tac ⟩
        intro k hk
        by_cases hprev : k < i.val
        · exact hmatch k hprev
        · have hki : k = i.val := by scalar_tac
          rw [hki, hhere, hthere, ← hsmall]
          exact heq
    · rename_i hend
      have hall : i.val = 9 := by scalar_tac
      simp only [WP.spec_ok, true_iff]
      refine ⟨ by omega, ?_ ⟩
      intro k hk
      rw [hfrom]
      exact hmatch k (by omega)
  · exact hinv

/-- `is_part_name` says yes exactly for a name that ends with the suffix of
a part file, in any letter case. It cannot fail, whatever the name. -/
@[step]
theorem artifact.path.is_part_name.spec (n : Slice U8) :
    artifact.path.is_part_name n ⦃ r => r = true ↔ PartName n.val ⦄ := by
  simp only [artifact.path.is_part_name]
  step as ⟨ suffix, hsuffix ⟩
  have hnine : suffix.val.length = 9 := by
    rw [hsuffix]
    scalar_tac
  split
  · rename_i hshort
    have : n.val.length < 9 := by scalar_tac
    simp only [WP.spec_ok]
    constructor
    · intro h
      cases h
    · intro hpart
      have := hpart.1
      omega
  · rename_i hlong
    step as ⟨ again, hagain ⟩
    have hnine' : again.val.length = 9 := by
      rw [hagain]
      scalar_tac
    step as ⟨ start, hstart ⟩
    exact artifact.path.is_part_name_loop.spec n start 0#usize
      ⟨ by scalar_tac, by scalar_tac, by intro k hk; simp at hk ⟩

theorem artifact.path.is_safe_name_loop.spec (n : Slice U8) (start : Usize)
    (hinv : start.val ≤ n.val.length ∧
      ∀ k x, k < start.val → n.val[k]? = some x → ¬ Separator x) :
    artifact.path.is_safe_name_loop n start
    ⦃ r => r = true ↔ ((∀ x ∈ n.val, ¬ Separator x) ∧ ¬ PartName n.val) ⦄ := by
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
      · rename_i hisbad
        -- The name holds a separator.
        simp only [WP.spec_ok]
        constructor
        · intro h
          cases h
        · intro hsafe
          have hmem : byte ∈ n.val := by
            rw [hbyte]
            exact List.getElem_mem hin
          exact absurd (hbad.mp hisbad) (hsafe.1 byte hmem)
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
      step as ⟨ part, hpart ⟩
      have hnone : ∀ x ∈ n.val, ¬ Separator x := by
        intro x hx
        obtain ⟨ k, hk ⟩ := List.mem_iff_getElem?.mp hx
        obtain ⟨ hklt, _ ⟩ := List.getElem?_eq_some_iff.mp hk
        exact hclean k x (by omega) hk
      rw [← hpart]
      simp only [decide_eq_true_eq]
      constructor
      · intro hnot
        exact ⟨ hnone, hnot ⟩
      · intro hboth
        exact hboth.2
  · exact hinv

/-- `is_safe_name` accepts exactly the safe names, and cannot fail. -/
@[step]
theorem artifact.path.is_safe_name.spec (n : Slice U8) :
    artifact.path.is_safe_name n ⦃ r => r = true ↔ SafeName n.val ⦄ := by
  simp only [artifact.path.is_safe_name]
  step as ⟨ empty, hempty ⟩
  have hnil : empty = true ↔ n.val = [] := by
    rw [hempty]
    simp [Slice.length]
  split
  · rename_i hisempty
    simp only [WP.spec_ok]
    constructor
    · intro h
      cases h
    · intro hsafe
      exact absurd (hnil.mp hisempty) hsafe.1
  · rename_i hnotempty
    split
    · rename_i hlong
      simp only [WP.spec_ok]
      constructor
      · intro h
        cases h
      · intro hsafe
        have := hsafe.2.1
        simp only [artifact.path.MAX_NAME_BYTES] at hlong
        scalar_tac
    · rename_i hshort
      step as ⟨ current, hcurrent ⟩
      split
      · rename_i hiscurrent
        simp only [WP.spec_ok]
        constructor
        · intro h
          cases h
        · intro hsafe
          exact absurd (hcurrent.mp hiscurrent) hsafe.2.2.1
      · rename_i hnotcurrent
        step as ⟨ parent, hparent ⟩
        split
        · rename_i hisparent
          simp only [WP.spec_ok]
          constructor
          · intro h
            cases h
          · intro hsafe
            exact absurd (hparent.mp hisparent) hsafe.2.2.2.1
        · rename_i hnotparent
          apply WP.spec_mono (artifact.path.is_safe_name_loop.spec n 0#usize
            ⟨ by scalar_tac, by intro k x hk; simp at hk ⟩)
          intro r hrest
          have hlength : n.val.length ≤ 246 := by
            simp only [artifact.path.MAX_NAME_BYTES] at hshort
            scalar_tac
          rw [hrest]
          constructor
          · intro hclean
            exact ⟨ fun hempty' => hnotempty (hnil.mpr hempty'), hlength,
              fun hdot => hnotcurrent (hcurrent.mpr hdot),
              fun hdots => hnotparent (hparent.mpr hdots), hclean.1, hclean.2 ⟩
          · intro hsafe
            exact ⟨ hsafe.2.2.2.2.1, hsafe.2.2.2.2.2 ⟩

/-! ## Whole paths -/

/-- Reading `rest`, with `current` the name read so far: every name is a
safe name. Names are what lies between one `/` and the next. -/
def AllSafe (current : List U8) : List U8 → Prop
  | [] => SafeName current
  | b :: rest =>
    if b = 47#u8 then SafeName current ∧ AllSafe [] rest else AllSafe (current ++ [b]) rest

/-- A path whose every name is a safe name. Each such name means one level
down, so the path stays inside the directory it is taken from. -/
def SafePath (path : List U8) : Prop := AllSafe [] path

/-- The bytes from `start` up to `stop`, with one more added, are the bytes
from `start` up to the next place. -/
theorem slice_snoc (p : List U8) (start stop : Nat) (x : U8)
    (hstart : start ≤ stop) (hstop : stop < p.length) (hx : x = p[stop]) :
    p.slice start stop ++ [x] = p.slice start (stop + 1) := by
  have hnext : stop + 1 - start = stop - start + 1 := by omega
  have hthere : (p.drop start)[stop - start]? = some x := by
    rw [List.getElem?_drop, hx]
    have hsame : start + (stop - start) = stop := by omega
    rw [hsame]
    exact List.getElem?_eq_getElem hstop
  simp only [List.slice]
  rw [hnext, List.take_add_one, hthere]
  rfl

/-- From a place to the end is the rest of the path. -/
theorem slice_to_end (p : List U8) (start : Nat) :
    p.slice start p.length = p.drop start := by
  simp only [List.slice]
  exact List.take_of_length_le (by simp)

theorem slice_empty (p : List U8) (start : Nat) : p.slice start start = [] := by
  simp [List.slice]

theorem artifact.path.is_safe_path_loop.spec (path : Slice U8) (begin at_ : Usize)
    (hinv : begin.val ≤ at_.val ∧ at_.val ≤ path.val.length ∧
      (AllSafe (path.val.slice begin.val at_.val) (path.val.drop at_.val) ↔
        SafePath path.val)) :
    artifact.path.is_safe_path_loop path begin at_
    ⦃ r => r = true ↔ SafePath path.val ⦄ := by
  unfold artifact.path.is_safe_path_loop
  apply loop.spec_decr_nat
    (measure := fun (p : Usize × Usize) => path.val.length - p.2.val)
    (inv := fun (p : Usize × Usize) => p.1.val ≤ p.2.val ∧ p.2.val ≤ path.val.length ∧
      (AllSafe (path.val.slice p.1.val p.2.val) (path.val.drop p.2.val) ↔
        SafePath path.val))
  · intro ⟨ start, i ⟩ hstate
    dsimp only at hstate
    obtain ⟨ hle, hin, hrest ⟩ := hstate
    simp only [artifact.path.is_safe_path_loop.body]
    split
    · rename_i hlt
      have hinside : i.val < path.val.length := by scalar_tac
      step as ⟨ byte, hbyte ⟩
      have hdrop := drop_at path.val i.val byte hinside hbyte
      split
      · rename_i hslash
        step as ⟨ piece, hpiece, hpiecelen ⟩
        step as ⟨ safe, hsafe ⟩
        -- What the path is, read from here: this name, and then the rest.
        have hhere : SafePath path.val ↔
            (SafeName piece.val ∧ AllSafe [] (path.val.drop (i.val + 1))) := by
          rw [← hrest, hdrop, hslash]
          simp only [AllSafe, ↓reduceIte]
          rw [← hpiece]
        split
        · rename_i haccepted
          step as ⟨ next, hnext ⟩
          refine ⟨ by scalar_tac, by scalar_tac, ?_, by scalar_tac ⟩
          rw [hnext, slice_empty, hhere]
          constructor
          · intro hafter
            exact ⟨ hsafe.mp haccepted, hafter ⟩
          · intro hboth
            exact hboth.2
        · rename_i hrejected
          simp only [WP.spec_ok]
          constructor
          · intro h
            cases h
          · intro hpath
            exact absurd (hsafe.mpr (hhere.mp hpath).1) hrejected
      · rename_i hnotslash
        step as ⟨ next, hnext ⟩
        refine ⟨ by scalar_tac, by scalar_tac, ?_, by scalar_tac ⟩
        rw [hnext, ← hrest, hdrop]
        simp only [AllSafe, hnotslash, ↓reduceIte]
        rw [slice_snoc path.val start.val i.val byte hle hinside hbyte]
    · rename_i hend
      have hall : i.val = path.val.length := by scalar_tac
      step as ⟨ piece, hpiece, hpiecelen ⟩
      step as ⟨ safe, hsafe ⟩
      rw [← hrest, hall, slice_to_end, List.drop_length]
      simp only [AllSafe]
      rw [← hpiece]
      exact hsafe
  · exact hinv

/-- `is_safe_path` accepts exactly the paths that are made of safe names
and nothing else, and cannot fail. -/
@[step]
theorem artifact.path.is_safe_path.spec (path : Slice U8) :
    artifact.path.is_safe_path path ⦃ r => r = true ↔ SafePath path.val ⦄ := by
  unfold artifact.path.is_safe_path
  exact artifact.path.is_safe_path_loop.spec path 0#usize 0#usize
    ⟨ by scalar_tac, by scalar_tac, by simp [SafePath, List.slice] ⟩

/-! ## Where a link points -/

/-- Following the name `piece` from `level` names below the root is allowed,
and leads to `next`. `..` goes one level up, and is not allowed at the root
itself. A safe name goes one level down. -/
def Follows (level : Nat) (piece : List U8) (next : Nat) : Prop :=
  (piece = [46#u8, 46#u8] ∧ next + 1 = level) ∨ (SafeName piece ∧ next = level + 1)

/-- Reading `rest`, with `current` the name read so far, from `level` names
below the root: each name may be followed from where the one before it led.
Levels are counted from the root, so none is ever above it. -/
def StaysInside (level : Nat) (current : List U8) : List U8 → Prop
  | [] => ∃ next, Follows level current next
  | b :: rest =>
    if b = 47#u8 then ∃ next, Follows level current next ∧ StaysInside next [] rest
    else StaysInside level (current ++ [b]) rest

/-- A name leads to one level and no other. -/
theorem Follows.unique {level : Nat} {piece : List U8} {a b : Nat}
    (ha : Follows level piece a) (hb : Follows level piece b) : a = b := by
  rcases ha with ⟨ hup, ha ⟩ | ⟨ hsafe, ha ⟩ <;> rcases hb with ⟨ hup', hb ⟩ | ⟨ hsafe', hb ⟩
  · omega
  · exact absurd hup hsafe'.2.2.2.1
  · exact absurd hup' hsafe.2.2.2.1
  · omega

/-- `follow` gives a level exactly where the name may be followed, and then
gives the level it leads to. -/
@[step]
theorem artifact.path.follow.spec (n : Slice U8) (level : U64) (hroom : level.val < U64.max) :
    artifact.path.follow n level
    ⦃ r =>
      (∀ next, r = some next →
        Follows level.val n.val next.val ∧ next.val ≤ level.val + 1) ∧
      (r = none → ∀ next, ¬ Follows level.val n.val next) ⦄ := by
  unfold artifact.path.follow
  step as ⟨ up, hup ⟩
  split
  · rename_i hisup
    have hdots := hup.mp hisup
    split
    · rename_i hroot
      have hzero : level.val = 0 := by scalar_tac
      simp only [WP.spec_ok]
      refine ⟨ fun next h => (by cases h), ?_ ⟩
      intro _ next hfollows
      rcases hfollows with ⟨ _, hlevel ⟩ | ⟨ hsafe, _ ⟩
      · omega
      · exact absurd hdots hsafe.2.2.2.1
    · rename_i hbelow
      step as ⟨ lower, hlower ⟩
      refine ⟨ ?_, fun h => by cases h ⟩
      intro next hnext
      have hsame : lower = next := Option.some.inj hnext
      rw [← hsame]
      exact ⟨ Or.inl ⟨ hdots, by scalar_tac ⟩, by scalar_tac ⟩
  · rename_i hnotup
    step as ⟨ safe, hsafe ⟩
    split
    · rename_i hissafe
      step as ⟨ deeper, hdeeper ⟩
      refine ⟨ ?_, fun h => by cases h ⟩
      intro next hnext
      have hsame : deeper = next := Option.some.inj hnext
      rw [← hsame]
      exact ⟨ Or.inr ⟨ hsafe.mp hissafe, by scalar_tac ⟩, by scalar_tac ⟩
    · rename_i hnotsafe
      simp only [WP.spec_ok]
      refine ⟨ fun next h => (by cases h), ?_ ⟩
      intro _ next hfollows
      rcases hfollows with ⟨ hdots, _ ⟩ | ⟨ hname, _ ⟩
      · exact hnotup (hup.mpr hdots)
      · exact hnotsafe (hsafe.mpr hname)

theorem artifact.path.is_safe_target_loop.spec (target : Slice U8) (level : U64)
    (begin at_ : Usize) (origin : Nat)
    (hroom : origin + target.val.length < U64.max)
    (hinv : begin.val ≤ at_.val ∧ at_.val ≤ target.val.length ∧
      level.val ≤ origin + at_.val ∧
      (StaysInside level.val (target.val.slice begin.val at_.val) (target.val.drop at_.val) ↔
        StaysInside origin [] target.val)) :
    artifact.path.is_safe_target_loop target level begin at_
    ⦃ r => r = true ↔ StaysInside origin [] target.val ⦄ := by
  unfold artifact.path.is_safe_target_loop
  apply loop.spec_decr_nat
    (measure := fun (p : U64 × Usize × Usize) => target.val.length - p.2.2.val)
    (inv := fun (p : U64 × Usize × Usize) =>
      p.2.1.val ≤ p.2.2.val ∧ p.2.2.val ≤ target.val.length ∧
      p.1.val ≤ origin + p.2.2.val ∧
      (StaysInside p.1.val (target.val.slice p.2.1.val p.2.2.val) (target.val.drop p.2.2.val) ↔
        StaysInside origin [] target.val))
  · intro ⟨ depth, start, i ⟩ hstate
    dsimp only at hstate
    obtain ⟨ hle, hin, hbound, hrest ⟩ := hstate
    simp only [artifact.path.is_safe_target_loop.body]
    split
    · rename_i hlt
      have hinside : i.val < target.val.length := by scalar_tac
      step as ⟨ byte, hbyte ⟩
      have hdrop := drop_at target.val i.val byte hinside hbyte
      split
      · rename_i hslash
        step as ⟨ piece, hpiece, hpiecelen ⟩
        -- What the target is, read from here: this name, and then the rest
        -- from wherever the name leads.
        have hhere : StaysInside origin [] target.val ↔
            ∃ next, Follows depth.val piece.val next ∧
              StaysInside next [] (target.val.drop (i.val + 1)) := by
          rw [← hrest, hdrop, hslash]
          simp only [StaysInside, ↓reduceIte]
          rw [← hpiece]
        step as ⟨ led, hled, hnowhere ⟩
        cases led with
        | none =>
          simp only [WP.spec_ok]
          constructor
          · intro h
            cases h
          · intro hstays
            obtain ⟨ next, hfollows, _ ⟩ := hhere.mp hstays
            exact absurd hfollows (hnowhere rfl next)
        | some next =>
          obtain ⟨ hfollows, hnextle ⟩ := hled next rfl
          dsimp only
          step as ⟨ after, hafter ⟩
          refine ⟨ by scalar_tac, by scalar_tac, by scalar_tac, ?_, by scalar_tac ⟩
          rw [hafter, slice_empty, hhere]
          constructor
          · intro hrestsafe
            exact ⟨ next.val, hfollows, hrestsafe ⟩
          · intro ⟨ other, hother, hrestsafe ⟩
            rw [Follows.unique hother hfollows] at hrestsafe
            exact hrestsafe
      · rename_i hnotslash
        step as ⟨ after, hafter ⟩
        refine ⟨ by scalar_tac, by scalar_tac, by scalar_tac, ?_, by scalar_tac ⟩
        rw [hafter, ← hrest, hdrop]
        simp only [StaysInside, hnotslash, ↓reduceIte]
        rw [slice_snoc target.val start.val i.val byte hle hinside hbyte]
    · rename_i hend
      have hall : i.val = target.val.length := by scalar_tac
      step as ⟨ piece, hpiece, hpiecelen ⟩
      have hhere : StaysInside origin [] target.val ↔ ∃ next, Follows depth.val piece.val next := by
        rw [← hrest, hall, slice_to_end, List.drop_length]
        simp only [StaysInside]
        rw [← hpiece]
      step as ⟨ led, hled, hnowhere ⟩
      cases led with
      | none =>
        constructor
        · intro h
          simp at h
        · intro hstays
          obtain ⟨ next, hfollows ⟩ := hhere.mp hstays
          exact absurd hfollows (hnowhere rfl next)
      | some next =>
        obtain ⟨ hfollows, _ ⟩ := hled next rfl
        constructor
        · intro _
          exact hhere.mpr ⟨ next.val, hfollows ⟩
        · intro _
          simp
  · exact hinv

/-- `is_safe_target` accepts exactly the targets that are not empty, not too
long, belong to a link below the root and, followed one name at a time from
the directory that holds the link, never climb above the root. The check
cannot fail. -/
@[step]
theorem artifact.path.is_safe_target.spec (target : Slice U8) (depth : U32) :
    artifact.path.is_safe_target target depth
    ⦃ r => r = true ↔
      (target.val ≠ [] ∧ target.val.length ≤ 4096 ∧ 0 < depth.val ∧
        StaysInside (depth.val - 1) [] target.val) ⦄ := by
  simp only [artifact.path.is_safe_target]
  step as ⟨ empty, hempty ⟩
  have hnil : empty = true ↔ target.val = [] := by
    rw [hempty]
    simp [Slice.length]
  split
  · rename_i hisempty
    simp only [WP.spec_ok]
    constructor
    · intro h
      cases h
    · intro hsafe
      exact absurd (hnil.mp hisempty) hsafe.1
  · rename_i hnotempty
    split
    · rename_i hlong
      simp only [WP.spec_ok]
      constructor
      · intro h
        cases h
      · intro hsafe
        have := hsafe.2.1
        simp only [artifact.path.MAX_TARGET_BYTES] at hlong
        scalar_tac
    · rename_i hshort
      have hlength : target.val.length ≤ 4096 := by
        simp only [artifact.path.MAX_TARGET_BYTES] at hshort
        scalar_tac
      split
      · rename_i hroot
        simp only [WP.spec_ok]
        constructor
        · intro h
          cases h
        · intro hsafe
          have := hsafe.2.2.1
          scalar_tac
      · rename_i hdeep
        have hpositive : 0 < depth.val := by scalar_tac
        step as ⟨ above, habove ⟩
        step as ⟨ level, hlevel ⟩
        have hstart :
            StaysInside level.val (target.val.slice (0#usize).val (0#usize).val)
              (target.val.drop (0#usize).val) ↔
            StaysInside (depth.val - 1) [] target.val := by
          simp [List.slice, hlevel, habove]
        apply WP.spec_mono (artifact.path.is_safe_target_loop.spec target level 0#usize
          0#usize (depth.val - 1) (by scalar_tac)
          ⟨ by scalar_tac, by scalar_tac, by scalar_tac, hstart ⟩)
        intro r hr
        rw [hr]
        constructor
        · intro hstays
          exact ⟨ fun hempty' => hnotempty (hnil.mpr hempty'), hlength, hpositive, hstays ⟩
        · intro hsafe
          exact hsafe.2.2.2

/-! ## The directory that holds an entry -/

theorem artifact.path.parent_len_loop.spec (path : Slice U8) (start : Usize)
    (hinv : start.val ≤ path.val.length ∧
      ∀ k x, start.val ≤ k → path.val[k]? = some x → x ≠ 47#u8) :
    artifact.path.parent_len_loop path start
    ⦃ r =>
      r.val ≤ path.val.length ∧
      (∀ k x, r.val < k → path.val[k]? = some x → x ≠ 47#u8) ∧
      (r.val = 0 ∨ path.val[r.val]? = some 47#u8) ⦄ := by
  unfold artifact.path.parent_len_loop
  apply loop.spec_decr_nat
    (measure := fun (i : Usize) => i.val)
    (inv := fun (i : Usize) => i.val ≤ path.val.length ∧
      ∀ k x, i.val ≤ k → path.val[k]? = some x → x ≠ 47#u8)
  · intro i hstate
    obtain ⟨ hle, hnone ⟩ := hstate
    simp only [artifact.path.parent_len_loop.body]
    split
    · rename_i hpositive
      step as ⟨ before, hbefore ⟩
      have hinside : before.val < path.val.length := by scalar_tac
      step as ⟨ byte, hbyte ⟩
      have hget : path.val[before.val]? = some byte := by
        rw [hbyte]
        exact List.getElem?_eq_getElem hinside
      split
      · rename_i hslash
        simp only [WP.spec_ok]
        refine ⟨ by scalar_tac, ?_, Or.inr (by rw [hget, hslash]) ⟩
        intro k x hk hx
        exact hnone k x (by scalar_tac) hx
      · rename_i hnotslash
        simp only [WP.spec_ok]
        refine ⟨ by scalar_tac, ?_, by scalar_tac ⟩
        intro k x hk hx
        by_cases hsame : k = before.val
        · rw [hsame, hget] at hx
          have hxb : byte = x := Option.some.inj hx
          rw [← hxb]
          exact hnotslash
        · exact hnone k x (by scalar_tac) hx
    · rename_i hzero
      have hstart : i.val = 0 := by scalar_tac
      simp only [WP.spec_ok]
      refine ⟨ by scalar_tac, ?_, Or.inl (by scalar_tac) ⟩
      intro k x hk hx
      exact hnone k x (by scalar_tac) hx
  · exact hinv

/-- `parent_len` is the place of the last `/`, or zero if there is none: no
`/` comes after it. What comes before it is the path of the directory that
holds the entry. -/
@[step]
theorem artifact.path.parent_len.spec (path : Slice U8) :
    artifact.path.parent_len path
    ⦃ r =>
      r.val ≤ path.val.length ∧
      (∀ k x, r.val < k → path.val[k]? = some x → x ≠ 47#u8) ∧
      (r.val = 0 ∨ path.val[r.val]? = some 47#u8) ⦄ := by
  simp only [artifact.path.parent_len]
  apply artifact.path.parent_len_loop.spec path (Slice.len path)
  refine ⟨ by scalar_tac, ?_ ⟩
  intro k x hk hx
  obtain ⟨ hklt, _ ⟩ := List.getElem?_eq_some_iff.mp hx
  scalar_tac

/-! ## How deep an entry lies -/

/-- How many `/` a path has. -/
def slashes : List U8 → Nat
  | [] => 0
  | b :: rest => (if b = 47#u8 then 1 else 0) + slashes rest

theorem slashes_append (a b : List U8) : slashes (a ++ b) = slashes a + slashes b := by
  induction a with
  | nil => simp [slashes]
  | cons x xs ih =>
    simp only [List.cons_append, slashes, ih]
    omega

/-- Reading one more byte adds one if it is a `/`. -/
theorem slashes_take_succ (p : List U8) (i : Nat) (x : U8) (hi : i < p.length) (hx : x = p[i]) :
    slashes (p.take (i + 1)) = slashes (p.take i) + (if x = 47#u8 then 1 else 0) := by
  have hthere : p[i]? = some x := by
    rw [hx]
    exact List.getElem?_eq_getElem hi
  rw [List.take_add_one, hthere, slashes_append]
  simp [slashes]

theorem artifact.path.depth_loop.spec (path : Slice U8) (names : U32) (start : Usize)
    (hinv : start.val ≤ path.val.length ∧
      names.val = min (slashes (path.val.take start.val) + 1) U32.max) :
    artifact.path.depth_loop path names start
    ⦃ r => r.val = min (slashes path.val + 1) U32.max ⦄ := by
  unfold artifact.path.depth_loop
  apply loop.spec_decr_nat
    (measure := fun (p : U32 × Usize) => path.val.length - p.2.val)
    (inv := fun (p : U32 × Usize) => p.2.val ≤ path.val.length ∧
      p.1.val = min (slashes (path.val.take p.2.val) + 1) U32.max)
  · intro ⟨ count, i ⟩ hstate
    dsimp only at hstate
    obtain ⟨ hle, hcount ⟩ := hstate
    simp only [artifact.path.depth_loop.body]
    split
    · rename_i hlt
      have hinside : i.val < path.val.length := by scalar_tac
      step as ⟨ byte, hbyte ⟩
      have hmore := slashes_take_succ path.val i.val byte hinside hbyte
      have hmax : (core.num.U32.MAX).val = U32.max := by
        simp [core.num.U32.MAX, U32.rMax, U32.max_eq]
      split
      · rename_i hslash
        split
        · rename_i hroom
          have hbelow : count.val < U32.max := by
            rw [← hmax]
            scalar_tac
          step as ⟨ more, hmoreval ⟩
          step as ⟨ next, hnext ⟩
          refine ⟨ by scalar_tac, ?_, by scalar_tac ⟩
          rw [hnext, hmore, hmoreval]
          simp only [hslash, ↓reduceIte]
          omega
        · rename_i hfull
          have hatmax : U32.max ≤ count.val := by
            rw [← hmax]
            scalar_tac
          step as ⟨ next, hnext ⟩
          refine ⟨ by scalar_tac, ?_, by scalar_tac ⟩
          rw [hnext, hmore]
          simp only [hslash, ↓reduceIte]
          omega
      · rename_i hnotslash
        step as ⟨ next, hnext ⟩
        refine ⟨ by scalar_tac, ?_, by scalar_tac ⟩
        rw [hnext, hmore]
        simp only [hnotslash, ↓reduceIte, Nat.add_zero]
        exact hcount
    · rename_i hend
      have hall : i.val = path.val.length := by scalar_tac
      simp only [WP.spec_ok]
      rw [hcount, hall, List.take_length]
  · exact hinv

/-- `depth` is the number of names in a path: none for the root, and
otherwise one more than it has `/`s. A count too large for the type stops at
the largest value, which no real path reaches. -/
@[step]
theorem artifact.path.depth.spec (path : Slice U8) :
    artifact.path.depth path
    ⦃ r =>
      (path.val = [] → r.val = 0) ∧
      (path.val ≠ [] → r.val = min (slashes path.val + 1) U32.max) ⦄ := by
  simp only [artifact.path.depth]
  step as ⟨ empty, hempty ⟩
  split
  · rename_i hisempty
    have hnil : path.val = [] := by
      have hzero : path.length = 0 := by
        rw [← hempty]
        exact hisempty
      simpa [Slice.length] using hzero
    simp only [WP.spec_ok]
    exact ⟨ fun _ => by scalar_tac, fun hne => absurd hnil hne ⟩
  · rename_i hnotempty
    have hone : (1#u32).val = min (slashes (path.val.take (0#usize).val) + 1) U32.max := by
      have hfits : 1 ≤ U32.max := by scalar_tac
      simp [slashes]
      omega
    apply WP.spec_mono (artifact.path.depth_loop.spec path 1#u32 0#usize
      ⟨ by scalar_tac, hone ⟩)
    intro r hr
    refine ⟨ fun hnil => ?_, fun _ => hr ⟩
    exfalso
    apply hnotempty
    rw [hempty]
    simp [Slice.length, hnil]

end model
