-- What is proven about `p2rent_core::artifact::bitfield`.
module
public import Aeneas
public import Mathlib.Tactic.IntervalCases
public import Std.Tactic.BVDecide
public import P2rentCore.Basics
@[expose] public section
open Aeneas Aeneas.Std Result

namespace p2rent_core

open artifact.bitfield

/-- The bytes are enough to hold every bit. -/
def artifact.bitfield.Bitfield.Sized (b : Bitfield) : Prop :=
  b.len.val ≤ 8 * b.bytes.val.length

/-- Whether bit `i` is set: it is in range, and its bit of its byte is 1. -/
def artifact.bitfield.Bitfield.bit (b : Bitfield) (i : Nat) : Bool :=
  decide (i < b.len.val) && (b.bytes.val.getD (i / 8) 0#u8).bv.getLsbD (i % 8)

/-- Masking a byte with a single bit leaves something exactly when that bit
was set. -/
theorem byte_and_bit (x : BitVec 8) (k : Nat) (hk : k < 8) :
    (x &&& (1#8 <<< k) != 0#8) = x.getLsbD k := by
  interval_cases k <;> bv_decide

/-- Or-ing a single bit into a byte sets that bit and no other. -/
theorem byte_or_bit (x : BitVec 8) (k j : Nat) (hk : k < 8) (hj : j < 8) :
    (x ||| (1#8 <<< k)).getLsbD j = (x.getLsbD j || decide (j = k)) := by
  interval_cases k <;> interval_cases j <;> bv_decide

/-- And-ing a byte with everything but a single bit clears that bit and no
other. -/
theorem byte_and_not_bit (x : BitVec 8) (k j : Nat) (hk : k < 8) (hj : j < 8) :
    (x &&& ~~~(1#8 <<< k)).getLsbD j = (x.getLsbD j && decide (j ≠ k)) := by
  interval_cases k <;> interval_cases j <;> bv_decide

/-- `get` never fails, and returns whether the bit is set. -/
@[step]
theorem artifact.bitfield.Bitfield.get.spec (b : Bitfield) (index : U64)
    (hb : b.Sized) :
    artifact.bitfield.Bitfield.get b index ⦃ r => r = b.bit index.val ⦄ := by
  unfold artifact.bitfield.Bitfield.get
  simp only [Bitfield.Sized] at hb
  split
  · rename_i hin
    step as ⟨ i, hi ⟩
    step as ⟨ slot, hfits, hover ⟩
    have hslot : slot.val = index.val / 8 := by
      have := hfits (by scalar_tac)
      omega
    step as ⟨ byte, hbyte ⟩
    step as ⟨ k, hk ⟩
    step as ⟨ mask, hmaskbv, hmaskval ⟩
    step as ⟨ masked, hmval, hmbv ⟩
    have hk8 : k.val < 8 := by omega
    have hne : (masked != 0#u8) = (masked.bv != 0#8) := by
      rw [Bool.eq_iff_iff]
      simp [UScalar.eq_equiv_bv_eq]
    have hbit := byte_and_bit byte.bv k.val hk8
    have hin' : index.val < b.len.val := by scalar_tac
    have hlen : index.val / 8 < b.bytes.val.length := by omega
    simp_all [Bitfield.bit]
  · rename_i hout
    simp [Bitfield.bit]
    scalar_tac

/-- Two positions are the same when they are the same bit of the same byte. -/
theorem same_position (j i : Nat) : (j / 8 = i / 8 ∧ j % 8 = i % 8) ↔ j = i := by
  omega

/-- `set` never fails, keeps the length, and sets exactly the one bit. An
index past the end changes nothing. -/
@[step]
theorem artifact.bitfield.Bitfield.set.spec (b : Bitfield) (index : U64)
    (hb : b.Sized) :
    artifact.bitfield.Bitfield.set b index
    ⦃ b' =>
      b'.len = b.len ∧ b'.bytes.val.length = b.bytes.val.length ∧
      ∀ j, b'.bit j = (b.bit j || decide (j = index.val ∧ index.val < b.len.val)) ⦄ := by
  unfold artifact.bitfield.Bitfield.set
  simp only [Bitfield.Sized] at hb
  split
  · rename_i hin
    have hin' : index.val < b.len.val := by scalar_tac
    step as ⟨ k, hk ⟩
    step as ⟨ mask, hmaskbv, hmaskval ⟩
    step as ⟨ i, hi ⟩
    step as ⟨ slot, hfits, hover ⟩
    have hslot : slot.val = index.val / 8 := by
      have := hfits (by scalar_tac)
      omega
    step as ⟨ byte, back, hbyte, hback ⟩
    step as ⟨ merged, hmval, hmbv ⟩
    have hk8 : k.val < 8 := by omega
    subst hback
    refine ⟨ by simp, ?_ ⟩
    intro j
    simp only [Bitfield.bit]
    by_cases hj : j < b.len.val
    · have hjlen : j / 8 < b.bytes.val.length := by omega
      by_cases hsame : j / 8 = index.val / 8
      · have hor := byte_or_bit byte.bv k.val (j % 8) hk8 (by omega)
        have hpos := same_position j index.val
        simp_all [List.getD_eq_getElem?_getD]
      · have hne : j ≠ index.val := by
          intro heq
          exact hsame (by rw [heq])
        simp_all [List.getD_eq_getElem?_getD]
    · have hne : j ≠ index.val := by omega
      have hfalse : decide (j < b.len.val) = false := by simpa using hj
      simp [hfalse, hne]
  · rename_i hout
    have : ¬ index.val < b.len.val := by scalar_tac
    simp [this]

/-- `clear` never fails, keeps the length, and unsets exactly the one bit.
An index past the end changes nothing. -/
@[step]
theorem artifact.bitfield.Bitfield.clear.spec (b : Bitfield) (index : U64)
    (hb : b.Sized) :
    artifact.bitfield.Bitfield.clear b index
    ⦃ b' =>
      b'.len = b.len ∧ b'.bytes.val.length = b.bytes.val.length ∧
      ∀ j, b'.bit j = (b.bit j && decide (j ≠ index.val)) ⦄ := by
  unfold artifact.bitfield.Bitfield.clear
  simp only [Bitfield.Sized] at hb
  split
  · rename_i hin
    have hin' : index.val < b.len.val := by scalar_tac
    step as ⟨ k, hk ⟩
    step as ⟨ mask, hmaskbv, hmaskval ⟩
    step as ⟨ others, hothers ⟩
    step as ⟨ i, hi ⟩
    step as ⟨ slot, hfits, hover ⟩
    have hslot : slot.val = index.val / 8 := by
      have := hfits (by scalar_tac)
      omega
    step as ⟨ byte, back, hbyte, hback ⟩
    step as ⟨ kept, hkval, hkbv ⟩
    have hk8 : k.val < 8 := by omega
    subst hback
    refine ⟨ by simp, ?_ ⟩
    intro j
    simp only [Bitfield.bit]
    by_cases hj : j < b.len.val
    · have hjlen : j / 8 < b.bytes.val.length := by omega
      by_cases hsame : j / 8 = index.val / 8
      · have hand := byte_and_not_bit byte.bv k.val (j % 8) hk8 (by omega)
        have hpos := same_position j index.val
        simp_all [List.getD_eq_getElem?_getD]
      · have hne : j ≠ index.val := by
          intro heq
          exact hsame (by rw [heq])
        simp_all [List.getD_eq_getElem?_getD]
    · have hfalse : decide (j < b.len.val) = false := by simpa using hj
      simp [hfalse]
  · rename_i hout
    have hpast : ¬ index.val < b.len.val := by scalar_tac
    simp only [WP.spec_ok, true_and]
    intro j
    simp only [Bitfield.bit]
    by_cases hj : j = index.val
    · subst hj
      simp [hpast]
    · simp [hj]

/-- `byte_len` is the number of bytes needed for that many bits, as long as
that number fits in a `usize`. -/
@[step]
theorem artifact.bitfield.byte_len.spec (bits : U64) (h : bits.val ≤ 8 * Usize.max) :
    artifact.bitfield.byte_len bits ⦃ r => r.val = (bits.val + 7) / 8 ⦄ := by
  unfold artifact.bitfield.byte_len
  step*

/-- `new` gives a bitfield of the asked length with no bit set. -/
@[step]
theorem artifact.bitfield.Bitfield.new.spec (len : U64) (h : len.val ≤ 8 * Usize.max) :
    artifact.bitfield.Bitfield.new len
    ⦃ b => b.len = len ∧ b.Sized ∧ ∀ j, b.bit j = false ⦄ := by
  unfold artifact.bitfield.Bitfield.new
  step as ⟨ count, hcount ⟩
  step as ⟨ bytes, hbytes, hlength ⟩
  refine ⟨ ?_, ?_ ⟩
  · simp only [Bitfield.Sized, hbytes, List.length_replicate]
    omega
  · intro j
    simp [Bitfield.bit, hbytes, List.getD_eq_getElem?_getD, List.getElem?_replicate]
    intro _
    split <;> simp

/-- Every bit of the byte 255 is set. -/
theorem all_ones (m : Nat) (hm : m < 8) : (255#u8).bv.getLsbD m = true := by
  interval_cases m <;> decide

/-- A byte whose value is `2^t - 1` has exactly its lowest `t` bits set. -/
theorem low_bits (x : U8) (t m : Nat) (hx : x.val = 2 ^ t - 1) :
    x.bv.getLsbD m = decide (m < t) := by
  have : x.bv.getLsbD m = x.val.testBit m := by
    simp [BitVec.getLsbD, UScalar.bv_toNat]
  rw [this, hx, Nat.testBit_two_pow_sub_one]

/-- `full` gives a bitfield of the asked length with exactly its own bits
set, and none past the end. -/
@[step]
theorem artifact.bitfield.Bitfield.full.spec (len : U64) (h : len.val ≤ 8 * Usize.max) :
    artifact.bitfield.Bitfield.full len
    ⦃ b => b.len = len ∧ b.Sized ∧ ∀ j, b.bit j = decide (j < len.val) ⦄ := by
  unfold artifact.bitfield.Bitfield.full
  step as ⟨ count, hcount ⟩
  step as ⟨ bytes, hbytes, hlength ⟩
  step as ⟨ tail, htail ⟩
  split
  · rename_i htailne
    have htail0 : tail.val ≠ 0 := by scalar_tac
    split
    · step as ⟨ mask, hmaskval, hmaskbits ⟩
      have hmask : mask.val = 2 ^ tail.val := by
        have : tail.val < 8 := by omega
        rw [hmaskval, Nat.one_shiftLeft]
        apply Nat.mod_eq_of_lt
        have : 2 ^ tail.val ≤ 2 ^ 7 := Nat.pow_le_pow_right (by norm_num) (by omega)
        scalar_tac
      have hmaskpos : 1 ≤ mask.val := by
        rw [hmask]
        exact Nat.one_le_two_pow
      step as ⟨ low, hlow ⟩
      step as ⟨ last, hlast ⟩
      step as ⟨ old, back, hold, hback ⟩
      subst hback
      refine ⟨ ?_, ?_ ⟩
      · simp only [Bitfield.Sized]
        simp [hbytes]
        scalar_tac
      · intro j
        simp only [Bitfield.bit]
        by_cases hj : j < len.val
        · have hlowbits := low_bits low tail.val (j % 8) (by scalar_tac)
          have hbefore : j / 8 < count.val := by scalar_tac
          have hlastval : last.val = count.val - 1 := by scalar_tac
          by_cases hlastbyte : j / 8 = last.val
          · have hlt : last.val < (List.replicate count.val 255#u8).length := by
              simp
              scalar_tac
            have hget : (bytes.set last low).val.getD (j / 8) 0#u8 = low := by
              simp [hlastbyte, List.getD_eq_getElem?_getD, hbytes,
                List.getElem?_set_self hlt]
            have hinside : j % 8 < tail.val := by omega
            rw [hget, hlowbits]
            simp [hj, hinside]
          · have hall := all_ones (j % 8) (by omega)
            have hne : last.val ≠ j / 8 := fun heq => hlastbyte heq.symm
            have hget : (bytes.set last low).val.getD (j / 8) 0#u8 = 255#u8 := by
              simp [List.getD_eq_getElem?_getD, hbytes, hbefore, List.getElem_set_ne hne]
            rw [hget, hall]
            simp [hj]
        · have hfalse : decide (j < len.val) = false := by simpa using hj
          simp [hfalse]
    · rename_i hempty
      scalar_tac
  · rename_i htailzero
    have htail0 : tail.val = 0 := by scalar_tac
    simp only [WP.spec_ok, true_and]
    refine ⟨ ?_, ?_ ⟩
    · simp only [Bitfield.Sized, hbytes, List.length_replicate]
      omega
    · intro j
      simp only [Bitfield.bit]
      by_cases hj : j < len.val
      · have hall := all_ones (j % 8) (by omega)
        have hbefore : j / 8 < count.val := by omega
        have hget : bytes.val.getD (j / 8) 0#u8 = 255#u8 := by
          simp [List.getD_eq_getElem?_getD, hbytes, hbefore]
        rw [hget, hall]
        simp [hj]
      · have hfalse : decide (j < len.val) = false := by simpa using hj
        simp [hfalse]

end p2rent_core
