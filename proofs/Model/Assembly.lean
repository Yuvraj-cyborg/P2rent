-- What is proven about `model::assembly`.
--
-- An assembly records which chunks of an artifact are in place and, for
-- each file, how many of its chunks are not. A download gives a file its
-- final name when `add` announces it.
--
--   * `add` announces a file only when every chunk of that file is in
--     place. This is the rule "a file is renamed only when it is whole".
--   * `add` announces a file whenever the chunk it was given was the last
--     one that file lacked, so no whole file is left without its name.
--   * `add` puts exactly the one chunk in place, and changes nothing if the
--     chunk was already there or its number names no chunk.
--   * `is_whole` says yes exactly when every chunk of the file is in place.
--   * The count each file keeps stays right through every operation, and no
--     operation can fail: none overflows or reads past the end of a list.
module
public import Aeneas
public import Model.Bitfield
public import Model.Layout
@[expose] public section
open Aeneas Aeneas.Std Result

namespace model

open artifact.bitfield artifact.layout assembly

/-! ## Counting the chunks that are not in place -/

/-- How many of the `n` chunks numbered from `first` are not in place. -/
def lackingFrom (held : Bitfield) (first : Nat) : Nat → Nat
  | 0 => 0
  | n + 1 => lackingFrom held first n + (if held.bit (first + n) then 0 else 1)

theorem lackingFrom_le (held : Bitfield) (first n : Nat) : lackingFrom held first n ≤ n := by
  induction n with
  | zero => simp [lackingFrom]
  | succ m ih =>
    simp only [lackingFrom]
    split <;> omega

/-- None is lacking exactly when every one is in place. -/
theorem lackingFrom_eq_zero (held : Bitfield) (first n : Nat) :
    lackingFrom held first n = 0 ↔
      ∀ i, first ≤ i → i < first + n → held.bit i = true := by
  induction n with
  | zero =>
    simp only [lackingFrom, true_iff]
    intro i hlo hhi
    omega
  | succ m ih =>
    simp only [lackingFrom]
    constructor
    · intro hzero i hlo hhi
      by_cases hlast : i = first + m
      · subst hlast
        by_cases hbit : held.bit (first + m) = true
        · exact hbit
        · have hunset : held.bit (first + m) = false := by simpa using hbit
          simp [hunset] at hzero
      · exact ih.mp (by omega) i hlo (by omega)
    · intro hall
      have hrest := ih.mpr (fun i hlo hhi => hall i hlo (by omega))
      have hlast := hall (first + m) (by omega) (by omega)
      simp [hrest, hlast]

/-- Putting one chunk in place, which was not, lowers the count by one if
the chunk is among those counted and leaves it alone if not. -/
theorem lackingFrom_set (held held' : Bitfield) (chunk first n : Nat)
    (hbits : ∀ i, held'.bit i = (held.bit i || decide (i = chunk)))
    (hwas : held.bit chunk = false) :
    lackingFrom held' first n + (if first ≤ chunk ∧ chunk < first + n then 1 else 0) =
      lackingFrom held first n := by
  induction n with
  | zero =>
    simp [lackingFrom]
  | succ m ih =>
    simp only [lackingFrom]
    by_cases hhere : first + m = chunk
    · -- The chunk is the last one counted, so it is not among the others.
      have hbefore : ¬ (first ≤ chunk ∧ chunk < first + m) := by omega
      have hnow : first ≤ chunk ∧ chunk < first + (m + 1) := by omega
      have hnew : held'.bit (first + m) = true := by
        rw [hbits, hhere]
        simp
      have hold : held.bit (first + m) = false := by
        rw [hhere]
        exact hwas
      simp only [hbefore, ↓reduceIte] at ih
      simp only [hnow, hnew, hold, ↓reduceIte]
      simp
      omega
    · have hsame : held'.bit (first + m) = held.bit (first + m) := by
        rw [hbits]
        simp [hhere]
      have hrange : (first ≤ chunk ∧ chunk < first + (m + 1)) ↔
          (first ≤ chunk ∧ chunk < first + m) := by omega
      simp only [hsame, hrange]
      omega

/-! ## The assembly -/

/-- The chunks of file `f` that are not in place. -/
def lackingIn (held : Bitfield) (l : Layout) (f : Nat) : Nat :=
  lackingFrom held (chunksBefore l.sizes.val l.chunk_size.val f)
    (chunksBefore l.sizes.val l.chunk_size.val (f + 1) -
      chunksBefore l.sizes.val l.chunk_size.val f)

/-- What `new` sets up and `add` keeps: the layout is consistent, there is
one bit for each chunk, and each file's count is the number of its chunks
that are not in place. -/
def assembly.Assembly.WellFormed (a : Assembly) : Prop :=
  a.layout.WellFormed ∧ a.held.Sized ∧ a.held.len = a.layout.chunk_count ∧
  a.missing.val.length = a.layout.sizes.val.length ∧
  ∀ f, f < a.layout.sizes.val.length →
    ∃ count, a.missing.val[f]? = some count ∧ count.val = lackingIn a.held a.layout f

/-- Every chunk of file `f` is in place. -/
def assembly.Assembly.Whole (a : Assembly) (f : Nat) : Prop :=
  ∀ i, chunksBefore a.layout.sizes.val a.layout.chunk_size.val f ≤ i →
    i < chunksBefore a.layout.sizes.val a.layout.chunk_size.val (f + 1) →
    a.held.bit i = true

/-- A file's count is zero exactly when the file is whole. -/
theorem lackingIn_eq_zero (a : Assembly) (f : Nat) :
    lackingIn a.held a.layout f = 0 ↔ a.Whole f := by
  have hmono := chunksBefore_mono a.layout.sizes.val a.layout.chunk_size.val
    (n := f) (m := f + 1) (by omega)
  simp only [lackingIn, assembly.Assembly.Whole, lackingFrom_eq_zero]
  constructor
  · intro h i hlo hhi
    exact h i hlo (by omega)
  · intro h i hlo hhi
    exact h i hlo (by omega)

/-- What is true each time round the inner loop of `new`: the count so far
covers the chunks from the file's first up to the one being looked at. -/
def countInv (held : Bitfield) (first : Nat) (stop : U64) (p : U64 × U64) : Prop :=
  first ≤ p.2.val ∧ p.2.val ≤ stop.val ∧
  p.1.val = lackingFrom held first (p.2.val - first)

theorem assembly.Assembly.new_loop0_loop0.spec (held : Bitfield) (stop lacking index : U64)
    (first : Nat) (hsized : held.Sized)
    (hinv : countInv held first stop (lacking, index)) :
    assembly.Assembly.new_loop0_loop0 held stop lacking index
    ⦃ r => r.val = lackingFrom held first (stop.val - first) ⦄ := by
  unfold assembly.Assembly.new_loop0_loop0
  apply loop.spec_decr_nat
    (measure := fun (p : U64 × U64) => stop.val - p.2.val)
    (inv := countInv held first stop)
  · intro ⟨ count, at_ ⟩ hstate
    simp only [countInv] at hstate
    obtain ⟨ hlo, hhi, hcount ⟩ := hstate
    simp only [assembly.Assembly.new_loop0_loop0.body]
    split
    · rename_i hlt
      have hbelow : at_.val < stop.val := by scalar_tac
      have hbound := lackingFrom_le held first (at_.val - first)
      have hnext : at_.val + 1 - first = at_.val - first + 1 := by omega
      have hthere : first + (at_.val - first) = at_.val := by omega
      step as ⟨ b, hb ⟩
      split
      · rename_i hheld
        have hbit : held.bit at_.val = true := by
          rw [← hb]
          exact hheld
        step as ⟨ next, hnextval ⟩
        refine ⟨ ⟨ by scalar_tac, by scalar_tac, ?_ ⟩, by scalar_tac ⟩
        dsimp only
        rw [hnextval, hnext]
        simp [lackingFrom, hthere, hbit, hcount]
      · rename_i hnotheld
        have hbit : held.bit at_.val = false := by
          rw [← hb]
          simpa using hnotheld
        step as ⟨ more, hmore ⟩
        step as ⟨ next, hnextval ⟩
        refine ⟨ ⟨ by scalar_tac, by scalar_tac, ?_ ⟩, by scalar_tac ⟩
        dsimp only
        rw [hnextval, hnext]
        simp [lackingFrom, hthere, hbit, hcount, hmore]
    · rename_i hstop
      have hall : at_.val = stop.val := by scalar_tac
      simp only [WP.spec_ok]
      rw [← hall]
      exact hcount
  · exact hinv

/-- What is true each time round the outer loop of `new`: every file so far
has its count. -/
def countsInv (l : Layout) (held : Bitfield) (p : alloc.vec.Vec U64 × Usize) : Prop :=
  p.2.val ≤ l.sizes.val.length ∧ p.1.val.length = p.2.val ∧
  ∀ f, f < p.2.val → ∃ count, p.1.val[f]? = some count ∧ count.val = lackingIn held l f

theorem assembly.Assembly.new_loop0.spec (l : Layout) (held : Bitfield)
    (missing : alloc.vec.Vec U64) (file : Usize)
    (hl : l.WellFormed) (hsized : held.Sized)
    (hinv : countsInv l held (missing, file)) :
    assembly.Assembly.new_loop0 l held missing file
    ⦃ r =>
      r.val.length = l.sizes.val.length ∧
      ∀ f, f < l.sizes.val.length →
        ∃ count, r.val[f]? = some count ∧ count.val = lackingIn held l f ⦄ := by
  unfold assembly.Assembly.new_loop0
  apply loop.spec_decr_nat
    (measure := fun (p : alloc.vec.Vec U64 × Usize) => l.sizes.val.length - p.2.val)
    (inv := countsInv l held)
  · intro ⟨ counts, at_ ⟩ hstate
    simp only [countsInv] at hstate
    obtain ⟨ hle, hlen, hcounts ⟩ := hstate
    simp only [assembly.Assembly.new_loop0.body]
    step as ⟨ files, hfiles ⟩
    split
    · rename_i hlt
      have hat : at_.val < l.sizes.val.length := by scalar_tac
      have hmono := chunksBefore_mono l.sizes.val l.chunk_size.val
        (n := at_.val) (m := at_.val + 1) (by omega)
      step as ⟨ start, stop, hstart, hstop ⟩
      have hbegin : countInv held start.val stop (0#u64, start) := by
        simp only [countInv]
        refine ⟨ by omega, by omega, ?_ ⟩
        simp [lackingFrom]
      step with assembly.Assembly.new_loop0_loop0.spec held stop 0#u64 start start.val
        hsized hbegin as ⟨ lacking, hlacking ⟩
      step as ⟨ counts', hcounts' ⟩
      step as ⟨ next, hnext ⟩
      refine ⟨ ⟨ by scalar_tac, by simp [hcounts', hlen, hnext], ?_ ⟩, by scalar_tac ⟩
      intro f hf
      rw [hcounts']
      by_cases hprev : f < at_.val
      · rw [List.getElem?_append_left (by omega)]
        exact hcounts f hprev
      · have hfat : f = at_.val := by scalar_tac
        subst hfat
        refine ⟨ lacking, ?_, ?_ ⟩
        · rw [← hlen]
          simp
        · rw [hlacking, hstart, hstop]
          rfl
    · rename_i hstop
      have hall : at_.val = l.sizes.val.length := by scalar_tac
      simp only [WP.spec_ok]
      refine ⟨ by omega, ?_ ⟩
      intro f hf
      exact hcounts f (by omega)
  · exact hinv

/-- `new` gives a consistent assembly of the layout. It starts from the
chunks it was given if there is one bit for each chunk of the layout, and
from none otherwise. -/
@[step]
theorem assembly.Assembly.new.spec (l : Layout) (held : Bitfield)
    (hl : l.WellFormed) (hsized : held.Sized) (hfits : l.chunk_count.val ≤ 8 * Usize.max) :
    assembly.Assembly.new l held
    ⦃ a =>
      a.WellFormed ∧ a.layout = l ∧
      (held.len = l.chunk_count → a.held = held) ∧
      (held.len ≠ l.chunk_count → ∀ i, a.held.bit i = false) ⦄ := by
  simp only [assembly.Assembly.new, artifact.bitfield.Bitfield.impl.len,
    artifact.layout.Layout.impl.chunk_count]
  -- Either way there is one bit for each chunk.
  have hstart :
      (if held.len = l.chunk_count then ok held
        else artifact.bitfield.Bitfield.new l.chunk_count)
      ⦃ h =>
        h.Sized ∧ h.len = l.chunk_count ∧
        (held.len = l.chunk_count → h = held) ∧
        (held.len ≠ l.chunk_count → ∀ i, h.bit i = false) ⦄ := by
    split
    · rename_i hsame
      rw [WP.spec_ok]
      exact ⟨ hsized, hsame, fun _ => rfl, fun hdiffer => absurd hsame hdiffer ⟩
    · rename_i hdiffer
      step as ⟨ empty, hlen, hemptysized, hnone ⟩
      exact ⟨ hemptysized, hlen, fun hsame => absurd hsame hdiffer, fun _ => hnone ⟩
  step with hstart as ⟨ start, hstartsized, hstartlen, hkept, hemptied ⟩
  step as ⟨ files, hfiles ⟩
  have hbegin : countsInv l start
      (alloc.vec.Vec.with_capacity U64 files, 0#usize) := by
    simp [countsInv, alloc.vec.Vec.with_capacity, alloc.vec.Vec.new]
  step with assembly.Assembly.new_loop0.spec l start _ 0#usize hl hstartsized hbegin
    as ⟨ missing, hlength, hcounts ⟩
  refine ⟨ ?_, hkept, hemptied ⟩
  simp only [assembly.Assembly.WellFormed]
  exact ⟨ hl, hstartsized, hstartlen, hlength, hcounts ⟩

/-- `is_whole` says yes exactly when the file exists and every chunk of it
is in place. -/
@[step]
theorem assembly.Assembly.is_whole.spec (a : Assembly) (file : Usize) (ha : a.WellFormed) :
    assembly.Assembly.is_whole a file
    ⦃ r => r = true ↔ (file.val < a.layout.sizes.val.length ∧ a.Whole file.val) ⦄ := by
  simp only [assembly.Assembly.is_whole]
  simp only [assembly.Assembly.WellFormed] at ha
  obtain ⟨ hl, hsized, hlen, hfiles, hcounts ⟩ := ha
  split
  · rename_i hin
    have hfile : file.val < a.layout.sizes.val.length := by scalar_tac
    step as ⟨ count, hcount ⟩
    have hval := val_of_getElem? (by omega) hcount (hcounts file.val hfile)
    have hzero : count = 0#u64 ↔ lackingIn a.held a.layout file.val = 0 := by
      rw [UScalar.eq_equiv, hval]
      rfl
    simp only [decide_eq_true_eq, hzero, lackingIn_eq_zero]
    exact ⟨ fun hwhole => ⟨ hfile, hwhole ⟩, fun hwhole => hwhole.2 ⟩
  · rename_i hout
    have hnone : ¬ file.val < a.layout.sizes.val.length := by scalar_tac
    simp only [WP.spec_ok]
    simp [hnone]

/-- `add` puts exactly the one chunk in place and keeps every count right.

It announces a file only when every chunk of that file is in place: a file
is given its final name only when it is whole. And it announces the file
whenever the chunk was the last one the file lacked, so a whole file is
never left without its name. -/
@[step]
theorem assembly.Assembly.add.spec (a : Assembly) (index : U64) (ha : a.WellFormed) :
    assembly.Assembly.add a index
    ⦃ announced a' =>
      a'.WellFormed ∧ a'.layout = a.layout ∧
      (∀ i, a'.held.bit i =
        (a.held.bit i || decide (i = index.val ∧ index.val < a.layout.chunk_count.val))) ∧
      match announced with
      | some file =>
        file.val < a.layout.sizes.val.length ∧ a.held.bit index.val = false ∧
        chunksBefore a.layout.sizes.val a.layout.chunk_size.val file.val ≤ index.val ∧
        index.val < chunksBefore a.layout.sizes.val a.layout.chunk_size.val (file.val + 1) ∧
        a'.Whole file.val
      | none =>
        ∀ f, f < a.layout.sizes.val.length →
          chunksBefore a.layout.sizes.val a.layout.chunk_size.val f ≤ index.val →
          index.val < chunksBefore a.layout.sizes.val a.layout.chunk_size.val (f + 1) →
          a.held.bit index.val = false → ¬ a'.Whole f ⦄ := by
  simp only [assembly.Assembly.add]
  have hwf := ha
  simp only [assembly.Assembly.WellFormed] at ha
  obtain ⟨ hl, hsized, hlen, hfiles, hcounts ⟩ := ha
  have hlayout := hl
  simp only [artifact.layout.Layout.WellFormed] at hlayout
  obtain ⟨ hc, -, -, hcount, - ⟩ := hlayout
  step as ⟨ b, hb ⟩
  split
  · rename_i hheld
    have hbit : a.held.bit index.val = true := by
      rw [← hb]
      exact hheld
    simp only [WP.spec_ok, WP.uncurry']
    refine ⟨ hwf, trivial, ?_, ?_ ⟩
    · intro i
      by_cases hi : i = index.val
      · subst hi
        simp [hbit]
      · simp [hi]
    · intro f _ _ _ hnot
      rw [hbit] at hnot
      exact absurd hnot (by simp)
  · rename_i hnotheld
    have hbit : a.held.bit index.val = false := by
      rw [← hb]
      simpa using hnotheld
    step as ⟨ located, hlocated ⟩
    cases located with
    | none =>
      have hpast : a.layout.chunk_count.val ≤ index.val := hlocated
      simp only [WP.spec_ok, WP.uncurry']
      refine ⟨ hwf, trivial, ?_, ?_ ⟩
      · intro i
        have hout : ¬ (i = index.val ∧ index.val < a.layout.chunk_count.val) := by omega
        simp [hout]
      · intro f hf hlo hhi _
        have hmono := chunksBefore_mono a.layout.sizes.val a.layout.chunk_size.val
          (n := f + 1) (m := a.layout.sizes.val.length) (by omega)
        exfalso
        omega
    | some chunk =>
      obtain ⟨ hinrange, hlocates ⟩ :
          index.val < a.layout.chunk_count.val ∧ a.layout.Locates index.val chunk := hlocated
      simp only [artifact.layout.Layout.Locates] at hlocates
      obtain ⟨ size, hsize, hlo, hhi, -, - ⟩ := hlocates
      have hfile := lt_length_of_getElem? hsize
      dsimp only
      step as ⟨ held', hlen', hbytes', hbits' ⟩
      have hbits : ∀ i, held'.bit i = (a.held.bit i || decide (i = index.val)) := by
        intro i
        have hin : index.val < a.held.len.val := by
          rw [hlen]
          exact hinrange
        rw [hbits' i]
        simp [hin]
      have hsized' : held'.Sized := by
        simp only [Bitfield.Sized] at hsized ⊢
        rw [hlen', hbytes']
        exact hsized
      step as ⟨ count, hcountget ⟩
      have hcountval := val_of_getElem? (by omega) hcountget (hcounts chunk.file.val hfile)
      have hmono := chunksBefore_mono a.layout.sizes.val a.layout.chunk_size.val
        (n := chunk.file.val) (m := chunk.file.val + 1) (by omega)
      -- The chunk is one of this file's, so the file's count goes down by
      -- one. In particular the count was not zero.
      have hdrop := lackingFrom_set a.held held' index.val
        (chunksBefore a.layout.sizes.val a.layout.chunk_size.val chunk.file.val)
        (chunksBefore a.layout.sizes.val a.layout.chunk_size.val (chunk.file.val + 1) -
          chunksBefore a.layout.sizes.val a.layout.chunk_size.val chunk.file.val) hbits hbit
      have hinside :
          chunksBefore a.layout.sizes.val a.layout.chunk_size.val chunk.file.val ≤ index.val ∧
          index.val < chunksBefore a.layout.sizes.val a.layout.chunk_size.val chunk.file.val +
            (chunksBefore a.layout.sizes.val a.layout.chunk_size.val (chunk.file.val + 1) -
              chunksBefore a.layout.sizes.val a.layout.chunk_size.val chunk.file.val) := by
        omega
      simp only [hinside, and_self, ↓reduceIte] at hdrop
      simp only [lackingIn] at hcountval
      step as ⟨ left, hleft ⟩
      step as ⟨ old, back, hold, hback ⟩
      subst hback
      have hbitsall : ∀ i, held'.bit i =
          (a.held.bit i || decide (i = index.val ∧ index.val < a.layout.chunk_count.val)) := by
        intro i
        rw [hbits i]
        simp [hinrange]
      -- Every count is still right: this file's went down with its chunk,
      -- and the chunk is no other file's.
      have hwf' : (assembly.Assembly.mk a.layout held'
          (a.missing.set chunk.file left)).WellFormed := by
        simp only [assembly.Assembly.WellFormed]
        refine ⟨ hl, hsized', by rw [hlen', hlen], by simp [hfiles], ?_ ⟩
        intro f hf
        by_cases hsame : f = chunk.file.val
        · subst hsame
          refine ⟨ left, ?_, ?_ ⟩
          · simp [List.getElem?_set_self (show chunk.file.val < a.missing.val.length by omega)]
          · simp only [lackingIn]
            omega
        · obtain ⟨ other, hother, hotherval ⟩ := hcounts f hf
          have hkeep := lackingFrom_set a.held held' index.val
            (chunksBefore a.layout.sizes.val a.layout.chunk_size.val f)
            (chunksBefore a.layout.sizes.val a.layout.chunk_size.val (f + 1) -
              chunksBefore a.layout.sizes.val a.layout.chunk_size.val f) hbits hbit
          have houtside :
              ¬ (chunksBefore a.layout.sizes.val a.layout.chunk_size.val f ≤ index.val ∧
                index.val < chunksBefore a.layout.sizes.val a.layout.chunk_size.val f +
                  (chunksBefore a.layout.sizes.val a.layout.chunk_size.val (f + 1) -
                    chunksBefore a.layout.sizes.val a.layout.chunk_size.val f)) := by
            intro hin
            exact hsame (file_of_chunk_unique a.layout.sizes.val a.layout.chunk_size.val
              f chunk.file.val index.val hin.1 (by omega) hlo hhi)
          simp only [houtside, ↓reduceIte, Nat.add_zero] at hkeep
          refine ⟨ other, ?_, ?_ ⟩
          · simp [List.getElem?_set_ne (Ne.symm hsame), hother]
          · rw [hotherval]
            simp only [lackingIn]
            omega
      have hwhole := lackingIn_eq_zero
        (assembly.Assembly.mk a.layout held' (a.missing.set chunk.file left)) chunk.file.val
      simp only [lackingIn] at hwhole
      split
      · rename_i hzero
        have hnone : left.val = 0 := by scalar_tac
        simp only [WP.spec_ok, WP.uncurry']
        exact ⟨ hwf', trivial, hbitsall, hfile, hbit, hlo, hhi, hwhole.mp (by omega) ⟩
      · rename_i hnonzero
        have hsome : left.val ≠ 0 := by scalar_tac
        simp only [WP.spec_ok, WP.uncurry']
        refine ⟨ hwf', trivial, hbitsall, ?_ ⟩
        intro f hf hflo hfhi _ hwholef
        -- The chunk's file is the only one that could have become whole,
        -- and it still lacks a chunk.
        have hsame : f = chunk.file.val :=
          file_of_chunk_unique a.layout.sizes.val a.layout.chunk_size.val
            f chunk.file.val index.val hflo hfhi hlo hhi
        subst hsame
        have := hwhole.mpr hwholef
        omega

end model
