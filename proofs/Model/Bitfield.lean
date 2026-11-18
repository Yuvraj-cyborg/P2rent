-- What is proven about `model::artifact::bitfield`.
module
public import Aeneas
public import Mathlib.Tactic.IntervalCases
public import Std.Tactic.BVDecide
public import Model.Basics
@[expose] public section
open Aeneas Aeneas.Std Result

namespace model

open artifact.bitfield

/-- The bytes are enough to hold every bit. -/
def artifact.bitfield.Bitfield.Sized (b : Bitfield) : Prop :=
  b.len.val ≤ 8 * b.bytes.val.length

/-- Whether bit `i` is set: it is in range, and its bit of its byte is 1. -/
def artifact.bitfield.Bitfield.bit (b : Bitfield) (i : Nat) : Bool :=
  decide (i < b.len.val) && (b.bytes.val.getD (i / 8) 0#u8).bv.getLsbD (i % 8)

/-- Bit `i` of the bytes, whether or not it is within the length. -/
def artifact.bitfield.Bitfield.raw (b : Bitfield) (i : Nat) : Bool :=
  (b.bytes.val.getD (i / 8) 0#u8).bv.getLsbD (i % 8)

/-- The bytes are exactly as many as the bits need, and no bit is set past
the end. `new` and `full` give such a bitfield, `set` and `clear` keep it
one, and it is what `is_well_formed` checks of one that came from
elsewhere. Counting the bits that are set relies on it: a bit set past the
end would be counted as one that is not there. -/
def artifact.bitfield.Bitfield.Tidy (b : Bitfield) : Prop :=
  b.bytes.val.length = (b.len.val + 7) / 8 ∧ ∀ i, b.len.val ≤ i → b.raw i = false

/-- A bit is set when it is in range and its bit of its byte is. -/
theorem artifact.bitfield.Bitfield.bit_eq (b : Bitfield) (i : Nat) :
    b.bit i = (decide (i < b.len.val) && b.raw i) := by
  rfl

/-- A tidy bitfield has room for every bit. -/
theorem artifact.bitfield.Bitfield.Tidy.sized {b : Bitfield} (h : b.Tidy) : b.Sized := by
  simp only [Bitfield.Sized]
  have := h.1
  omega

/-- In a tidy bitfield a bit is set exactly when its bit of its byte is. -/
theorem artifact.bitfield.Bitfield.Tidy.bit_eq_raw {b : Bitfield} (h : b.Tidy) (i : Nat) :
    b.bit i = b.raw i := by
  rw [Bitfield.bit_eq]
  by_cases hi : i < b.len.val
  · simp [hi]
  · have := h.2 i (by omega)
    simp [hi, this]

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
index past the end changes nothing. A tidy bitfield stays tidy. -/
@[step]
theorem artifact.bitfield.Bitfield.set.spec (b : Bitfield) (index : U64)
    (hb : b.Sized) :
    artifact.bitfield.Bitfield.set b index
    ⦃ b' =>
      b'.len = b.len ∧ b'.bytes.val.length = b.bytes.val.length ∧
      (∀ j, b'.bit j = (b.bit j || decide (j = index.val ∧ index.val < b.len.val))) ∧
      (b.Tidy → b'.Tidy) ⦄ := by
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
    refine ⟨ by simp, ?_, ?_ ⟩
    · intro j
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
    · -- The bit that is set is within the length, so nothing past the end
      -- changes.
      intro htidy
      refine ⟨ by simpa using htidy.1, ?_ ⟩
      intro i hi
      have hpast := htidy.2 i (by simpa using hi)
      have hilen : b.len.val ≤ i := by simpa using hi
      simp only [Bitfield.raw] at hpast ⊢
      by_cases hsame : i / 8 = index.val / 8
      · have hor := byte_or_bit byte.bv k.val (i % 8) hk8 (by omega)
        have hne : i % 8 ≠ k.val := by omega
        have hjlen : i / 8 < b.bytes.val.length := by omega
        simp_all [List.getD_eq_getElem?_getD]
      · simp_all [List.getD_eq_getElem?_getD]
  · rename_i hout
    have : ¬ index.val < b.len.val := by scalar_tac
    simp [this]

/-- `clear` never fails, keeps the length, and unsets exactly the one bit.
An index past the end changes nothing. A tidy bitfield stays tidy. -/
@[step]
theorem artifact.bitfield.Bitfield.clear.spec (b : Bitfield) (index : U64)
    (hb : b.Sized) :
    artifact.bitfield.Bitfield.clear b index
    ⦃ b' =>
      b'.len = b.len ∧ b'.bytes.val.length = b.bytes.val.length ∧
      (∀ j, b'.bit j = (b.bit j && decide (j ≠ index.val))) ∧
      (b.Tidy → b'.Tidy) ⦄ := by
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
    refine ⟨ by simp, ?_, ?_ ⟩
    · intro j
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
    · -- Unsetting a bit sets none.
      intro htidy
      refine ⟨ by simpa using htidy.1, ?_ ⟩
      intro i hi
      have hpast := htidy.2 i (by simpa using hi)
      have hilen : b.len.val ≤ i := by simpa using hi
      simp only [Bitfield.raw] at hpast ⊢
      by_cases hsame : i / 8 = index.val / 8
      · have hand := byte_and_not_bit byte.bv k.val (i % 8) hk8 (by omega)
        have hjlen : i / 8 < b.bytes.val.length := by omega
        simp_all [List.getD_eq_getElem?_getD]
      · simp_all [List.getD_eq_getElem?_getD]
  · rename_i hout
    have hpast : ¬ index.val < b.len.val := by scalar_tac
    simp only [WP.spec_ok, true_and]
    refine ⟨ ?_, fun htidy => htidy ⟩
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

/-- `new` gives a tidy bitfield of the asked length with no bit set. -/
@[step]
theorem artifact.bitfield.Bitfield.new.spec (len : U64) (h : len.val ≤ 8 * Usize.max) :
    artifact.bitfield.Bitfield.new len
    ⦃ b => b.len = len ∧ b.Sized ∧ (∀ j, b.bit j = false) ∧ b.Tidy ⦄ := by
  unfold artifact.bitfield.Bitfield.new
  step as ⟨ count, hcount ⟩
  step as ⟨ bytes, hbytes, hlength ⟩
  refine ⟨ ?_, ?_, ?_, ?_ ⟩
  · simp only [Bitfield.Sized, hbytes, List.length_replicate]
    omega
  · intro j
    simp [Bitfield.bit, hbytes, List.getD_eq_getElem?_getD, List.getElem?_replicate]
    intro _
    split <;> simp
  · simp only [hbytes, List.length_replicate]
    omega
  · intro i _
    simp [Bitfield.raw, hbytes, List.getD_eq_getElem?_getD, List.getElem?_replicate]
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

/-- `full` gives a tidy bitfield of the asked length with exactly its own
bits set: none past the end, in the bytes themselves. -/
@[step]
theorem artifact.bitfield.Bitfield.full.spec (len : U64) (h : len.val ≤ 8 * Usize.max) :
    artifact.bitfield.Bitfield.full len
    ⦃ b => b.len = len ∧ b.Sized ∧ (∀ j, b.bit j = decide (j < len.val)) ∧ b.Tidy ⦄ := by
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
      have hlastval : last.val = count.val - 1 := by scalar_tac
      refine ⟨ ?_, ?_, ?_, ?_ ⟩
      · simp only [Bitfield.Sized]
        simp [hbytes]
        scalar_tac
      rotate_left
      · simp [hbytes]
        scalar_tac
      · -- Past the end there is the masked part of the last byte, and
        -- then no byte at all.
        intro i hi
        have hilen : len.val ≤ i := by simpa using hi
        simp only [Bitfield.raw]
        by_cases hlastbyte : i / 8 = last.val
        · have hlowbits := low_bits low tail.val (i % 8) (by scalar_tac)
          have hlt : last.val < (List.replicate count.val 255#u8).length := by
            simp
            scalar_tac
          have hget : (bytes.set last low).val.getD (i / 8) 0#u8 = low := by
            simp [hlastbyte, List.getD_eq_getElem?_getD, hbytes,
              List.getElem?_set_self hlt]
          have houtside : ¬ i % 8 < tail.val := by omega
          rw [hget, hlowbits]
          simp [houtside]
        · have hbeyond : count.val ≤ i / 8 := by omega
          simp [List.getD_eq_getElem?_getD, hbytes, hbeyond]
      · intro j
        simp only [Bitfield.bit]
        by_cases hj : j < len.val
        · have hlowbits := low_bits low tail.val (j % 8) (by scalar_tac)
          have hbefore : j / 8 < count.val := by scalar_tac
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
    refine ⟨ ?_, ?_, ?_, ?_ ⟩
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
    · simp only [hbytes, List.length_replicate]
      omega
    · -- The bits fill their bytes, so past the end there is no byte.
      intro i hi
      have hilen : len.val ≤ i := by simpa using hi
      have hbeyond : count.val ≤ i / 8 := by omega
      simp [Bitfield.raw, List.getD_eq_getElem?_getD, hbytes, hbeyond]

/-! ## Bitfields that came from elsewhere, and counting -/

/-- `is_empty` says whether the bitfield has no bits at all. -/
@[step]
theorem artifact.bitfield.Bitfield.is_empty.spec (b : Bitfield) :
    artifact.bitfield.Bitfield.is_empty b ⦃ r => r = decide (b.len.val = 0) ⦄ := by
  unfold artifact.bitfield.Bitfield.is_empty
  simp only [WP.spec_ok]
  by_cases h : b.len = 0#u64
  · simp [h]
  · have : b.len.val ≠ 0 := by scalar_tac
    simp [h, this]

/-- Shifting a byte down by `t` leaves nothing exactly when none of its
bits from `t` up is set. -/
theorem byte_shifted_out (x : BitVec 8) (t : Nat) :
    x >>> t = 0#8 ↔ ∀ m, t ≤ m → x.getLsbD m = false := by
  constructor
  · intro h m hm
    have hbit : (x >>> t).getLsbD (m - t) = false := by simp [h]
    rw [BitVec.getLsbD_ushiftRight] at hbit
    have : t + (m - t) = m := by omega
    rw [this] at hbit
    exact hbit
  · intro h
    apply BitVec.eq_of_getLsbD_eq
    intro i _
    rw [BitVec.getLsbD_ushiftRight]
    simp [h (t + i) (by omega)]

/-- `is_well_formed` says yes exactly when the bitfield has the expected
length and is tidy: as many bytes as its bits need, and no bit set past the
end. It cannot fail. The length has to be one whose bytes can be counted in
a `usize`. -/
@[step]
theorem artifact.bitfield.Bitfield.is_well_formed.spec (b : Bitfield) (expected : U64)
    (h : expected.val ≤ 8 * Usize.max) :
    artifact.bitfield.Bitfield.is_well_formed b expected
    ⦃ r => r = true ↔ (b.len = expected ∧ b.Tidy) ⦄ := by
  unfold artifact.bitfield.Bitfield.is_well_formed
  split
  · rename_i hdiffer
    have : b.len ≠ expected := by simpa using hdiffer
    simp [this]
  · rename_i hsame
    have hlen : b.len = expected := by scalar_tac
    step as ⟨ count, hcount ⟩
    split
    · rename_i hbytes
      have : b.bytes.val.length ≠ (b.len.val + 7) / 8 := by scalar_tac
      simp [Bitfield.Tidy, this]
    · rename_i hbytes
      have hlength : b.bytes.val.length = (b.len.val + 7) / 8 := by scalar_tac
      step as ⟨ tail, htail ⟩
      split
      · rename_i hwhole
        have htail0 : b.len.val % 8 = 0 := by scalar_tac
        simp only [WP.spec_ok, true_iff]
        refine ⟨ hlen, hlength, ?_ ⟩
        intro i hi
        have hbeyond : b.bytes.val.length ≤ i / 8 := by omega
        simp [Bitfield.raw, List.getD_eq_getElem?_getD, hbeyond]
      · rename_i hpart
        have htailne : b.len.val % 8 ≠ 0 := by scalar_tac
        split
        · rename_i hnone
          scalar_tac
        · rename_i hsome
          step as ⟨ last, hlast ⟩
          step as ⟨ byte, hbyte ⟩
          step as ⟨ shifted, hshifted, hshiftedbv ⟩
          have hlastlt : last.val < b.bytes.val.length := by scalar_tac
          have hlastval : last.val = b.len.val / 8 := by scalar_tac
          have hget : b.bytes.val.getD last.val 0#u8 = byte := by
            simp [List.getD_eq_getElem?_getD, hlastlt, hbyte]
          have hzero : shifted = 0#u8 ↔ byte.bv >>> tail.val = 0#8 := by
            rw [← hshiftedbv]
            simp [UScalar.eq_equiv_bv_eq]
          simp only [decide_eq_true_eq, hzero, byte_shifted_out]
          constructor
          · -- Past the end there is the rest of the last byte, and then no
            -- byte at all.
            intro hclear
            refine ⟨ hlen, hlength, ?_ ⟩
            intro i hi
            simp only [Bitfield.raw]
            by_cases hin : i / 8 < b.bytes.val.length
            · have hbyteidx : i / 8 = last.val := by omega
              rw [hbyteidx, hget]
              exact hclear (i % 8) (by omega)
            · have hbeyond : b.bytes.val.length ≤ i / 8 := by omega
              simp [List.getD_eq_getElem?_getD, hbeyond]
          · intro ⟨ _, _, hpast ⟩ m hm
            have hbit := hpast (8 * last.val + m) (by omega)
            simp only [Bitfield.raw] at hbit
            by_cases hm8 : m < 8
            · have hdiv : (8 * last.val + m) / 8 = last.val := by omega
              have hmod : (8 * last.val + m) % 8 = m := by omega
              rw [hdiv, hmod, hget] at hbit
              exact hbit
            · exact BitVec.getLsbD_of_ge _ _ (by omega)

/-- How many of the first `n` bits of the bytes are set. -/
def artifact.bitfield.Bitfield.rawOnes (b : Bitfield) (n : Nat) : Nat :=
  (List.range n).countP (fun i => b.raw i)

/-- How many bits are set. -/
def artifact.bitfield.Bitfield.ones (b : Bitfield) : Nat :=
  (List.range b.len.val).countP (fun i => b.bit i)

/-- The numbers below `m` are those below `n` and then the rest. -/
theorem range_split (n m : Nat) (h : n ≤ m) :
    List.range m = List.range n ++ (List.range (m - n)).map (fun i => n + i) := by
  have hm : m = n + (m - n) := by omega
  conv_lhs => rw [hm]
  exact List.range_add

/-- The bits set among the first `m` are those among the first `n` and
those after them. -/
theorem artifact.bitfield.Bitfield.rawOnes_split (b : Bitfield) (n m : Nat) (h : n ≤ m) :
    b.rawOnes m = b.rawOnes n + (List.range (m - n)).countP (fun i => b.raw (n + i)) := by
  simp only [Bitfield.rawOnes, range_split n m h, List.countP_append, List.countP_map]
  rfl

/-- Looking at more bits never finds fewer set. -/
theorem artifact.bitfield.Bitfield.rawOnes_mono (b : Bitfield) {n m : Nat} (h : n ≤ m) :
    b.rawOnes n ≤ b.rawOnes m := by
  rw [b.rawOnes_split n m h]
  omega

/-- One more byte adds the bits that are set in it. -/
theorem artifact.bitfield.Bitfield.rawOnes_byte (b : Bitfield) (k : Nat)
    (hk : k < b.bytes.val.length) :
    b.rawOnes (8 * (k + 1)) = b.rawOnes (8 * k) + popCount (b.bytes.val[k]).bv := by
  rw [b.rawOnes_split (8 * k) (8 * (k + 1)) (by omega)]
  have hwidth : 8 * (k + 1) - 8 * k = 8 := by omega
  rw [hwidth, popCount]
  congr 1
  apply List.countP_congr
  intro i hi
  have hi8 : i < 8 := List.mem_range.mp hi
  have hdiv : (8 * k + i) / 8 = k := by omega
  have hmod : (8 * k + i) % 8 = i := by omega
  simp [Bitfield.raw, hdiv, hmod, List.getD_eq_getElem?_getD, hk]

/-- In a tidy bitfield every bit that is set in the bytes is one of its
own, however far the bytes are looked through. -/
theorem artifact.bitfield.Bitfield.Tidy.rawOnes_eq_ones {b : Bitfield} (h : b.Tidy)
    {n : Nat} (hn : b.len.val ≤ n) : b.rawOnes n = b.ones := by
  rw [b.rawOnes_split b.len.val n hn]
  have hnone : (List.range (n - b.len.val)).countP (fun i => b.raw (b.len.val + i)) = 0 := by
    rw [List.countP_eq_zero]
    intro i _
    simp [h.2 (b.len.val + i) (by omega)]
  rw [hnone, Nat.add_zero]
  simp only [Bitfield.rawOnes, Bitfield.ones]
  apply List.countP_congr
  intro i hi
  have hilt : i < b.len.val := List.mem_range.mp hi
  simp [Bitfield.bit_eq, hilt]

/-- No more bits are set than there are. -/
theorem artifact.bitfield.Bitfield.ones_le (b : Bitfield) : b.ones ≤ b.len.val := by
  have := List.countP_le_length (p := fun i => b.bit i) (l := List.range b.len.val)
  simpa [Bitfield.ones] using this

/-- `u8::count_ones` gives the number of bits set in the byte. -/
@[step]
theorem core.num.U8.count_ones.spec (x : U8) :
    core.num.U8.count_ones x ⦃ r => r.val = popCount x.bv ⦄ := by
  unfold core.num.U8.count_ones
  simp only [WP.spec_ok]
  simp

theorem artifact.bitfield.Bitfield.count_ones_loop.spec (b : Bitfield) (total : U64)
    (byte : Usize) (hb : b.Tidy)
    (hinv : byte.val ≤ b.bytes.val.length ∧ total.val = b.rawOnes (8 * byte.val)) :
    artifact.bitfield.Bitfield.count_ones_loop b total byte ⦃ r => r.val = b.ones ⦄ := by
  unfold artifact.bitfield.Bitfield.count_ones_loop
  apply loop.spec_decr_nat
    (measure := fun (p : U64 × Usize) => b.bytes.val.length - p.2.val)
    (inv := fun (p : U64 × Usize) =>
      p.2.val ≤ b.bytes.val.length ∧ p.1.val = b.rawOnes (8 * p.2.val))
  · intro ⟨ sofar, at_ ⟩ hstate
    obtain ⟨ hle, hsofar ⟩ := hstate
    dsimp only at hle hsofar
    simp only [artifact.bitfield.Bitfield.count_ones_loop.body]
    split
    · rename_i hlt
      have hbelow : at_.val < b.bytes.val.length := by scalar_tac
      -- With this byte counted, the count is still no more than the bits
      -- there are, so it cannot overflow.
      have hbyte := b.rawOnes_byte at_.val hbelow
      have hmono := b.rawOnes_mono (n := 8 * (at_.val + 1)) (m := 8 * b.bytes.val.length)
        (by omega)
      have hall := hb.rawOnes_eq_ones (n := 8 * b.bytes.val.length) (by
        have := hb.1
        omega)
      have hfew := b.ones_le
      step as ⟨ here, hhere ⟩
      step as ⟨ ones, hones ⟩
      step as ⟨ wide, hwide ⟩
      have hlenmax : b.len.val ≤ U64.max := by scalar_tac
      have hroom : sofar.val + wide.val ≤ U64.max := by
        rw [hwide, hones, hhere, hsofar, ← hbyte]
        omega
      step as ⟨ more, hmore ⟩
      step as ⟨ next, hnext ⟩
      refine ⟨ by scalar_tac, ?_, by scalar_tac ⟩
      rw [hnext, hbyte, hmore, hwide, hones, hsofar, hhere]
    · rename_i hdone
      have hat : at_.val = b.bytes.val.length := by scalar_tac
      simp only [WP.spec_ok]
      rw [hsofar, hat]
      exact hb.rawOnes_eq_ones (by
        have := hb.1
        omega)
  · exact hinv

/-- `count_ones` gives the number of bits that are set, for a tidy
bitfield, and cannot fail or overflow. -/
@[step]
theorem artifact.bitfield.Bitfield.count_ones.spec (b : Bitfield) (hb : b.Tidy) :
    artifact.bitfield.Bitfield.count_ones b ⦃ r => r.val = b.ones ⦄ := by
  unfold artifact.bitfield.Bitfield.count_ones
  exact artifact.bitfield.Bitfield.count_ones_loop.spec b 0#u64 0#usize hb
    (by simp [Bitfield.rawOnes])

/-- `is_full` says yes exactly when every bit is set, for a tidy bitfield. -/
@[step]
theorem artifact.bitfield.Bitfield.is_full.spec (b : Bitfield) (hb : b.Tidy) :
    artifact.bitfield.Bitfield.is_full b
    ⦃ r => r = true ↔ ∀ i, i < b.len.val → b.bit i = true ⦄ := by
  unfold artifact.bitfield.Bitfield.is_full
  step as ⟨ count, hcount ⟩
  have hsame : count = b.len ↔ b.ones = b.len.val := by
    rw [← hcount]
    scalar_tac
  have hall := List.countP_eq_length (p := fun i => b.bit i) (l := List.range b.len.val)
  simp only [decide_eq_true_eq, hsame, Bitfield.ones]
  rw [List.length_range] at hall
  rw [hall]
  simp [List.mem_range]

end model
