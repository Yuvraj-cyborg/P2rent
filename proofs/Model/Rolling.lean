-- What is proven about `model::rolling`.
--
-- A rolling hash is the hash of a window of bytes that can be moved one
-- byte on without reading the window again. The search for chunks that
-- have moved inside a file leans on one thing: the hash that was rolled to
-- a place is the hash that would have been made there from nothing.
--
--   * `of` gives the hash the definition below names, for any window.
--   * `roll` turns the hash of a window into the hash of the window one
--     byte on: exactly what `of` gives for that window. So a hash rolled
--     through a file is, at every step, the hash of the window it has
--     reached.
--   * Neither can fail: nothing overflows, and no byte is read past the
--     end of the window.
module
public import Aeneas
public import Model.Funs
@[expose] public section
open Aeneas Aeneas.Std Result

namespace model

open rolling

/-! ## What the hash of a window is -/

/-- The factor, as the 64-bit number the arithmetic is done on. -/
def factor : BitVec 64 := rolling.FACTOR.bv

/-- A byte, as one of those numbers. -/
def wide (byte : U8) : BitVec 64 := byte.bv.setWidth 64

/-- The hash of `bytes` carried on from `start`: for each byte, times the
factor, plus the byte. -/
def hashFrom (start : BitVec 64) : List U8 → BitVec 64
  | [] => start
  | byte :: rest => hashFrom (start * factor + wide byte) rest

/-- The hash of a window. -/
def hashOf (bytes : List U8) : BitVec 64 := hashFrom 0 bytes

/-- What was carried in is multiplied by the factor once for each byte. -/
theorem hashFrom_split (start : BitVec 64) (bytes : List U8) :
    hashFrom start bytes = start * factor ^ bytes.length + hashFrom 0 bytes := by
  induction bytes generalizing start with
  | nil => simp [hashFrom]
  | cons byte rest ih =>
    simp only [hashFrom, List.length_cons]
    rw [ih (start * factor + wide byte), ih (0 * factor + wide byte)]
    ring

/-- One more byte at the end: times the factor, plus the byte. -/
theorem hashFrom_append (start : BitVec 64) (bytes : List U8) (byte : U8) :
    hashFrom start (bytes ++ [byte]) = hashFrom start bytes * factor + wide byte := by
  induction bytes generalizing start with
  | nil => simp [hashFrom]
  | cons first rest ih =>
    simp only [List.cons_append, hashFrom]
    exact ih _

/-- The arithmetic of moving a window on: take out what the first byte
came to, multiply by the factor, add the new byte. -/
theorem hashOf_moved_on (out inn : U8) (rest : List U8) :
    (hashOf (out :: rest) - factor ^ rest.length * wide out) * factor + wide inn
      = hashOf (rest ++ [inn]) := by
  simp only [hashOf, hashFrom]
  rw [hashFrom_split (0 * factor + wide out) rest, hashFrom_append]
  ring

/-! ## `of` -/

/-- What is true each time round the loop of `of`: the bytes still to come,
carried on from the hash so far, give the hash of the whole window, and
the other number is the factor to the power of one less than the bytes
done. -/
def ofInv (window : Slice U8) (p : U64 × U64 × Usize) : Prop :=
  p.2.2.val ≤ window.val.length ∧
  hashFrom p.1.bv (window.val.drop p.2.2.val) = hashOf window.val ∧
  p.2.1.bv = factor ^ (p.2.2.val - 1)

theorem rolling.Rolling.of_loop.spec (window : Slice U8) (hash leaving : U64) (at_ : Usize)
    (hinv : ofInv window (hash, leaving, at_)) :
    rolling.Rolling.of_loop window hash leaving at_
    ⦃ r => r.1.bv = hashOf window.val ∧ r.2.bv = factor ^ (window.val.length - 1) ⦄ := by
  unfold rolling.Rolling.of_loop
  apply loop.spec_decr_nat
    (measure := fun (p : U64 × U64 × Usize) => window.val.length - p.2.2.val)
    (inv := ofInv window)
  · intro ⟨ h, l, i ⟩ hstate
    simp only [ofInv] at hstate
    obtain ⟨ hle, hhash, hleaving ⟩ := hstate
    simp only [rolling.Rolling.of_loop.body]
    split
    · rename_i hlt
      have hbelow : i.val < window.val.length := by scalar_tac
      have hdrop : window.val.drop i.val = window.val[i.val] :: window.val.drop (i.val + 1) :=
        List.drop_eq_getElem_cons hbelow
      simp only [lift]
      step as ⟨ byte, hbyte ⟩
      split
      · rename_i hpos
        step as ⟨ next, hnext ⟩
        refine ⟨ ⟨ by scalar_tac, ?_, ?_ ⟩, by scalar_tac ⟩
        · rw [hnext, ← hhash, hdrop]
          simp [hashFrom, factor, wide, hbyte]
        · have hone : next.val - 1 = (i.val - 1) + 1 := by scalar_tac
          rw [hone, pow_succ, ← hleaving]
          simp [factor]
      · rename_i hzero
        step as ⟨ next, hnext ⟩
        have hi : i.val = 0 := by scalar_tac
        refine ⟨ ⟨ by scalar_tac, ?_, ?_ ⟩, by scalar_tac ⟩
        · rw [hnext, ← hhash, hdrop]
          simp [hashFrom, factor, wide, hbyte]
        · have hone : next.val - 1 = 0 := by scalar_tac
          rw [hone, hleaving, hi]
    · rename_i hend
      have hall : i.val = window.val.length := by scalar_tac
      simp only [WP.spec_ok]
      refine ⟨ ?_, by rw [hleaving, hall] ⟩
      rw [← hhash, hall]
      simp [hashFrom]
  · exact hinv

/-- `of` gives the hash of the window, and the factor to the power of one
less than its length. It cannot fail. -/
theorem rolling.Rolling.of.spec (window : Slice U8) :
    rolling.Rolling.of window
    ⦃ r => r.hash.bv = hashOf window.val ∧
      r.leaving.bv = factor ^ (window.val.length - 1) ⦄ := by
  unfold rolling.Rolling.of
  have hinv : ofInv window (0#u64, 1#u64, 0#usize) := by
    simp [ofInv, hashOf]
  step with rolling.Rolling.of_loop.spec window 0#u64 1#u64 0#usize hinv as ⟨ hash, leaving, hhash, hleaving ⟩
  exact ⟨ hhash, hleaving ⟩

/-! ## `roll` -/

/-- A value that was only wrapped up is that value. -/
theorem lift_is {α : Type} (x : α) : lift x ⦃ y => y = x ⦄ := by
  simp only [lift, WP.spec_ok]

/-- Rolled on by one byte, the hash of a window is the hash of the next
window: the one without its first byte and with `inn` at its end. The other
number stays what `of` gives for a window of that length. So what comes out
is what `of` gives for the next window, and `roll` cannot fail. -/
theorem rolling.Rolling.roll.spec (self : rolling.Rolling) (out inn : U8) (rest : List U8)
    (hhash : self.hash.bv = hashOf (out :: rest))
    (hleaving : self.leaving.bv = factor ^ rest.length) :
    rolling.Rolling.roll self out inn
    ⦃ r => r.hash.bv = hashOf (rest ++ [inn]) ∧ r.leaving.bv = factor ^ rest.length ⦄ := by
  unfold rolling.Rolling.roll
  step with lift_is as ⟨ i, hi ⟩
  step with lift_is as ⟨ i1, hi1 ⟩
  step with lift_is as ⟨ without, hwithout ⟩
  step with lift_is as ⟨ i2, hi2 ⟩
  step with lift_is as ⟨ i3, hi3 ⟩
  step with lift_is as ⟨ i4, hi4 ⟩
  subst_vars
  refine ⟨ ?_, hleaving ⟩
  rw [← hashOf_moved_on out inn rest, ← hhash, ← hleaving]
  simp [factor, wide]

end model
