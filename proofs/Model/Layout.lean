-- What is proven about `model::artifact::layout`.
--
-- A layout cuts a list of files into chunks. Each file is cut on its own,
-- and the chunks are numbered across all the files, in file order.
--
--   * `new` fails only when the chunk size is zero or the sizes add up to
--     more than a `u64` holds. What it returns is consistent: each file's
--     first chunk number is the number of chunks in the files before it.
--   * `locate` returns nothing for an index past the last chunk. For any
--     other index it names a chunk that lies wholly inside one file, starts
--     on a chunk boundary, is not empty, and is a whole chunk unless it is
--     the last of its file.
--   * `locate` and `file_chunks` agree on which file a chunk belongs to.
--   * Every byte of every file lies in exactly one chunk.
--   * The chunk lengths add up to the total size.
--   * On a layout built by `new`, none of these can fail: none overflows,
--     divides by zero or reads past the end of a list.
module
public import Aeneas
public import Model.Basics
@[expose] public section
open Aeneas Aeneas.Std Result

namespace model

open artifact.layout

/-! ## Chunks of one file -/

/-- Chunks needed for a file of `size` bytes: the size divided by the chunk
size, rounded up. -/
def chunksIn (size chunk : Nat) : Nat :=
  if size % chunk = 0 then size / chunk else size / chunk + 1

/-- The length of chunk `k` of a file: a whole chunk, or what is left of the
file if that is less. -/
def chunkLen (size chunk k : Nat) : Nat :=
  min chunk (size - k * chunk)

/-- A number lies between the multiple of the chunk size at or below it and
the next one. -/
theorem div_mul_bounds (b chunk : Nat) (hc : 0 < chunk) :
    b / chunk * chunk ≤ b ∧ b < b / chunk * chunk + chunk := by
  have hsplit := Nat.div_add_mod b chunk
  have hrest := Nat.mod_lt b hc
  have hcomm := Nat.mul_comm chunk (b / chunk)
  omega

/-- Only one chunk of a file covers a given byte. -/
theorem chunk_of_byte_unique (b chunk j k : Nat)
    (hjlo : j * chunk ≤ b) (hjhi : b < j * chunk + chunk)
    (hklo : k * chunk ≤ b) (hkhi : b < k * chunk + chunk) : j = k := by
  by_cases hless : j < k
  · have hmul : (j + 1) * chunk ≤ k * chunk := Nat.mul_le_mul_right chunk hless
    rw [Nat.add_mul] at hmul
    omega
  · by_cases hmore : k < j
    · have hmul : (k + 1) * chunk ≤ j * chunk := Nat.mul_le_mul_right chunk hmore
      rw [Nat.add_mul] at hmul
      omega
    · omega

/-- Chunk `k` of a file exists exactly when it starts before the file
ends. -/
theorem lt_chunksIn_iff (size chunk k : Nat) (hc : 0 < chunk) :
    k < chunksIn size chunk ↔ k * chunk < size := by
  have hsplit := Nat.div_add_mod size chunk
  have hrest := Nat.mod_lt size hc
  have hcomm := Nat.mul_comm chunk (size / chunk)
  unfold chunksIn
  by_cases hmod : size % chunk = 0
  · rw [ite_eq_left hmod]
    constructor
    · intro hk
      have hmul : (k + 1) * chunk ≤ size / chunk * chunk := Nat.mul_le_mul_right chunk hk
      rw [Nat.add_mul] at hmul
      omega
    · intro hk
      by_cases hlt : k < size / chunk
      · exact hlt
      · have hmul : size / chunk * chunk ≤ k * chunk :=
          Nat.mul_le_mul_right chunk (by omega)
        omega
  · rw [ite_eq_right hmod]
    constructor
    · intro hk
      have hmul : k * chunk ≤ size / chunk * chunk := Nat.mul_le_mul_right chunk (by omega)
      omega
    · intro hk
      by_cases hlt : k < size / chunk + 1
      · exact hlt
      · have hmul : (size / chunk + 1) * chunk ≤ k * chunk :=
          Nat.mul_le_mul_right chunk (by omega)
        rw [Nat.add_mul] at hmul
        omega

/-- A file has no more chunks than bytes. -/
theorem chunksIn_le (size chunk : Nat) (hc : 0 < chunk) : chunksIn size chunk ≤ size := by
  by_cases h : size < chunksIn size chunk
  · have hlt := (lt_chunksIn_iff size chunk size hc).mp h
    have hmul : size * 1 ≤ size * chunk := Nat.mul_le_mul_left size hc
    omega
  · omega

/-- A file's chunks are enough to hold it. -/
theorem le_chunksIn_mul (size chunk : Nat) (hc : 0 < chunk) :
    size ≤ chunksIn size chunk * chunk := by
  by_cases h : chunksIn size chunk * chunk < size
  · have := (lt_chunksIn_iff size chunk (chunksIn size chunk) hc).mpr h
    omega
  · omega

/-- The first `n` chunks of a file hold `n` whole chunks, or the whole file
if that is less. -/
theorem chunkLens_sum (size chunk n : Nat) (hc : 0 < chunk) (hn : n ≤ chunksIn size chunk) :
    ((List.range n).map (chunkLen size chunk)).sum = min (n * chunk) size := by
  induction n with
  | zero => simp
  | succ m ih =>
    have hprev := ih (by omega)
    have hstart := (lt_chunksIn_iff size chunk m hc).mp (by omega)
    rw [List.range_succ, List.map_append, List.sum_append, hprev]
    simp only [List.map_cons, List.map_nil, List.sum_cons, List.sum_nil, chunkLen]
    rw [Nat.add_mul]
    omega

/-- The lengths of a file's chunks add up to the file's size. -/
theorem file_chunkLens_sum (size chunk : Nat) (hc : 0 < chunk) :
    ((List.range (chunksIn size chunk)).map (chunkLen size chunk)).sum = size := by
  rw [chunkLens_sum size chunk (chunksIn size chunk) hc (Nat.le_refl _)]
  exact Nat.min_eq_right (le_chunksIn_mul size chunk hc)

/-! ## Sums over the files before a given one -/

/-- The sum of `f` over the first `n` files. -/
def sumBefore (f : U64 → Nat) (sizes : List U64) (n : Nat) : Nat :=
  ((sizes.take n).map f).sum

theorem sumBefore_succ (f : U64 → Nat) (sizes : List U64) (n : Nat) (size : U64)
    (h : sizes[n]? = some size) :
    sumBefore f sizes (n + 1) = sumBefore f sizes n + f size := by
  simp [sumBefore, List.take_add_one, h]

theorem sumBefore_past (f : U64 → Nat) (sizes : List U64) (n : Nat) (h : sizes[n]? = none) :
    sumBefore f sizes (n + 1) = sumBefore f sizes n := by
  simp [sumBefore, List.take_add_one, h]

theorem sumBefore_le_succ (f : U64 → Nat) (sizes : List U64) (n : Nat) :
    sumBefore f sizes n ≤ sumBefore f sizes (n + 1) := by
  cases h : sizes[n]? with
  | none => rw [sumBefore_past f sizes n h]
  | some size =>
    rw [sumBefore_succ f sizes n size h]
    exact Nat.le_add_right _ _

/-- Taking more files never makes the sum smaller. -/
theorem sumBefore_mono (f : U64 → Nat) (sizes : List U64) {n m : Nat} (h : n ≤ m) :
    sumBefore f sizes n ≤ sumBefore f sizes m := by
  induction m with
  | zero =>
    have hn : n = 0 := by omega
    subst hn
    exact Nat.le_refl _
  | succ k ih =>
    by_cases hk : n ≤ k
    · exact Nat.le_trans (ih hk) (sumBefore_le_succ f sizes k)
    · have hn : n = k + 1 := by omega
      subst hn
      exact Nat.le_refl _

/-- A sum of smaller numbers is smaller. -/
theorem sumBefore_le (f g : U64 → Nat) (h : ∀ x, f x ≤ g x) (sizes : List U64) (n : Nat) :
    sumBefore f sizes n ≤ sumBefore g sizes n := by
  induction n with
  | zero => simp [sumBefore]
  | succ k ih =>
    cases hk : sizes[k]? with
    | none =>
      rw [sumBefore_past f sizes k hk, sumBefore_past g sizes k hk]
      exact ih
    | some size =>
      rw [sumBefore_succ f sizes k size hk, sumBefore_succ g sizes k size hk]
      have := h size
      omega

/-- The chunks in the first `n` files. This is the number of the first
chunk of file `n`. -/
def chunksBefore (sizes : List U64) (chunk n : Nat) : Nat :=
  sumBefore (fun size => chunksIn size.val chunk) sizes n

/-- The bytes in the first `n` files. -/
def bytesBefore (sizes : List U64) (n : Nat) : Nat :=
  sumBefore (fun size => size.val) sizes n

theorem chunksBefore_succ (sizes : List U64) (chunk n : Nat) (size : U64)
    (h : sizes[n]? = some size) :
    chunksBefore sizes chunk (n + 1) = chunksBefore sizes chunk n + chunksIn size.val chunk :=
  sumBefore_succ _ sizes n size h

theorem chunksBefore_mono (sizes : List U64) (chunk : Nat) {n m : Nat} (h : n ≤ m) :
    chunksBefore sizes chunk n ≤ chunksBefore sizes chunk m :=
  sumBefore_mono _ sizes h

theorem bytesBefore_succ (sizes : List U64) (n : Nat) (size : U64)
    (h : sizes[n]? = some size) :
    bytesBefore sizes (n + 1) = bytesBefore sizes n + size.val :=
  sumBefore_succ _ sizes n size h

theorem bytesBefore_mono (sizes : List U64) {n m : Nat} (h : n ≤ m) :
    bytesBefore sizes n ≤ bytesBefore sizes m :=
  sumBefore_mono _ sizes h

/-- A chunk belongs to one file only. -/
theorem file_of_chunk_unique (sizes : List U64) (chunk f g index : Nat)
    (hf : chunksBefore sizes chunk f ≤ index) (hf' : index < chunksBefore sizes chunk (f + 1))
    (hg : chunksBefore sizes chunk g ≤ index) (hg' : index < chunksBefore sizes chunk (g + 1)) :
    f = g := by
  by_cases hbefore : f < g
  · have := chunksBefore_mono sizes chunk (n := f + 1) (m := g) (by omega)
    omega
  · by_cases hafter : g < f
    · have := chunksBefore_mono sizes chunk (n := g + 1) (m := f) (by omega)
      omega
    · omega

/-- Files have no more chunks than bytes, so if the bytes fit in a `u64`
the chunks do. -/
theorem chunksBefore_le_bytesBefore (sizes : List U64) (chunk n : Nat) (hc : 0 < chunk) :
    chunksBefore sizes chunk n ≤ bytesBefore sizes n :=
  sumBefore_le _ _ (fun size => chunksIn_le size.val chunk hc) sizes n

/-- A place inside a list holds something. -/
theorem lt_length_of_getElem? {xs : List U64} {i : Nat} {x : U64} (h : xs[i]? = some x) :
    i < xs.length := by
  by_cases hi : i < xs.length
  · exact hi
  · rw [List.getElem?_eq_none (by omega)] at h
    exact absurd h (by simp)

/-- Reading a list at a place where its value is known. -/
theorem val_of_getElem? {xs : List U64} {i n : Nat} {x : U64} (hi : i < xs.length)
    (hx : x = xs[i]) (h : ∃ y, xs[i]? = some y ∧ y.val = n) : x.val = n := by
  obtain ⟨ y, hy, hval ⟩ := h
  rw [List.getElem?_eq_getElem hi] at hy
  have heq : xs[i] = y := Option.some.inj hy
  rw [hx, heq, hval]

/-! ## The layout -/

/-- What `new` sets up and every other function relies on: the chunk size is
not zero, each file's first chunk number is the number of chunks in the files
before it, and the two totals are right. -/
def artifact.layout.Layout.WellFormed (l : Layout) : Prop :=
  0 < l.chunk_size.val ∧
  l.first_chunk.val.length = l.sizes.val.length ∧
  (∀ f, f < l.sizes.val.length →
    ∃ first, l.first_chunk.val[f]? = some first ∧
      first.val = chunksBefore l.sizes.val l.chunk_size.val f) ∧
  l.chunk_count.val = chunksBefore l.sizes.val l.chunk_size.val l.sizes.val.length ∧
  l.total_size.val = bytesBefore l.sizes.val l.sizes.val.length

/-- `chunks_in` rounds up, and cannot overflow. -/
@[step]
theorem artifact.layout.chunks_in.spec (size chunk_size : U64) (hc : 0 < chunk_size.val) :
    artifact.layout.chunks_in size chunk_size
    ⦃ r => r.val = chunksIn size.val chunk_size.val ⦄ := by
  unfold artifact.layout.chunks_in
  have hle := chunksIn_le size.val chunk_size.val hc
  unfold chunksIn at hle ⊢
  step as ⟨ whole, hwhole ⟩
  step as ⟨ rest, hrest ⟩
  split
  · rename_i hzero
    have hmod : size.val % chunk_size.val = 0 := by scalar_tac
    simp only [WP.spec_ok]
    rw [ite_eq_left hmod]
    exact hwhole
  · rename_i hnonzero
    have hmod : ¬ size.val % chunk_size.val = 0 := by scalar_tac
    rw [ite_eq_right hmod] at hle ⊢
    step as ⟨ r, hr ⟩
    scalar_tac

/-- `to_u32` keeps a number that fits, and clamps one that does not. It
never fails. -/
@[step]
theorem artifact.layout.to_u32.spec (n : U64) :
    artifact.layout.to_u32 n
    ⦃ r => (n.val ≤ U32.max → r.val = n.val) ∧ (U32.max < n.val → r.val = U32.max) ⦄ := by
  unfold artifact.layout.to_u32
  step as ⟨ r, hfits, hover ⟩
  by_cases h : n.val ≤ U32.max
  · obtain ⟨x, hx, hval⟩ := hfits h
    simp [hx, hval]
    scalar_tac
  · have hr := hover (by omega)
    simp [hr]
    exact ⟨ fun hle => absurd hle h, fun _ => by simp [U32.rMax, U32.max_eq] ⟩

/-- What is true each time round the loop in `new`, once some files have
been laid out: their first chunk numbers are recorded, and the two running
totals cover exactly those files. -/
def layoutInv (sizes : Slice U64) (chunk_size : U32)
    (p : alloc.vec.Vec U64 × U64 × U64 × Usize) : Prop :=
  p.2.2.2.val ≤ sizes.val.length ∧
  p.1.val.length = p.2.2.2.val ∧
  (∀ f, f < p.2.2.2.val →
    ∃ first, p.1.val[f]? = some first ∧
      first.val = chunksBefore sizes.val chunk_size.val f) ∧
  p.2.1.val = chunksBefore sizes.val chunk_size.val p.2.2.2.val ∧
  p.2.2.1.val = bytesBefore sizes.val p.2.2.2.val

theorem artifact.layout.Layout.new_loop.spec (sizes : Slice U64) (chunk_size : U32)
    (first_chunk : alloc.vec.Vec U64) (chunk_count total_size : U64) (file : Usize)
    (hc : 0 < chunk_size.val)
    (hinv : layoutInv sizes chunk_size (first_chunk, chunk_count, total_size, file)) :
    artifact.layout.Layout.new_loop sizes chunk_size first_chunk chunk_count total_size file
    ⦃ r =>
      match r with
      | none => U64.max < bytesBefore sizes.val sizes.val.length
      | some l => l.chunk_size = chunk_size ∧ l.sizes.val = sizes.val ∧ l.WellFormed ⦄ := by
  unfold artifact.layout.Layout.new_loop
  apply loop.spec_decr_nat
    (measure := fun (p : alloc.vec.Vec U64 × U64 × U64 × Usize) =>
      sizes.val.length - p.2.2.2.val)
    (inv := layoutInv sizes chunk_size)
  · intro ⟨ firsts, count, total, at_ ⟩ hstate
    simp only [layoutInv] at hstate
    obtain ⟨ hle, hlen, hfirst, hcount, htotal ⟩ := hstate
    simp only [artifact.layout.Layout.new_loop.body]
    split
    · rename_i hlt
      have hat : at_.val < sizes.val.length := by scalar_tac
      step as ⟨ size, hsize ⟩
      have hget : sizes.val[at_.val]? = some size := by
        rw [hsize]
        exact List.getElem?_eq_getElem hat
      have hchunks := chunksBefore_succ sizes.val chunk_size.val at_.val size hget
      have hbytes := bytesBefore_succ sizes.val at_.val size hget
      have hmono := bytesBefore_mono sizes.val (n := at_.val + 1) (m := sizes.val.length)
        (by omega)
      have hfew := chunksBefore_le_bytesBefore sizes.val chunk_size.val (at_.val + 1) hc
      step as ⟨ firsts', hfirsts' ⟩
      step as ⟨ wide, hwide ⟩
      step as ⟨ chunks, hchunksval ⟩
      rw [hwide] at hchunksval
      step as ⟨ sum, hsum ⟩
      step as ⟨ counted, hcounted ⟩
      cases sum with
      | none =>
        have hover : U64.max < total.val + size.val := hsum
        simp only [WP.spec_ok]
        omega
      | some total' =>
        obtain ⟨ hsumfits, htotal', - ⟩ :
            total.val + size.val ≤ U64.max ∧ total'.val = total.val + size.val ∧
              total'.bv = total.bv + size.bv := hsum
        cases counted with
        | none =>
          -- The chunks cannot overflow when the bytes did not.
          have hover : U64.max < count.val + chunks.val := hcounted
          omega
        | some count' =>
          obtain ⟨ -, hcount', - ⟩ :
              count.val + chunks.val ≤ U64.max ∧ count'.val = count.val + chunks.val ∧
                count'.bv = count.bv + chunks.bv := hcounted
          step as ⟨ next, hnext ⟩
          refine ⟨ ⟨ by scalar_tac, by simp [hfirsts', hlen, hnext], ?_, ?_, ?_ ⟩,
            by scalar_tac ⟩
          · intro f hf
            rw [hfirsts']
            by_cases hprev : f < at_.val
            · rw [List.getElem?_append_left (by omega)]
              exact hfirst f hprev
            · have hfat : f = at_.val := by scalar_tac
              subst hfat
              refine ⟨ count, ?_, hcount ⟩
              rw [← hlen]
              simp
          · dsimp only
            rw [hnext, hchunks]
            omega
          · dsimp only
            rw [hnext, hbytes]
            omega
    · rename_i hstop
      have hall : at_.val = sizes.val.length := by scalar_tac
      have hclone : ∀ x ∈ sizes.val, core.clone.CloneU64.clone x = ok x := by
        intro x _
        rfl
      step with alloc.slice.Slice.to_vec_spec core.clone.CloneU64 sizes hclone
        as ⟨ copy, hcopy ⟩
      have hval : copy.val = sizes.val := by
        rw [hcopy]
        rfl
      refine ⟨ hval, ?_ ⟩
      simp only [artifact.layout.Layout.WellFormed, hval]
      rw [← hall]
      exact ⟨ hc, hlen, hfirst, hcount, htotal ⟩
  · exact hinv

/-- `new` fails only when the chunk size is zero or the sizes add up to more
than a `u64` holds. Otherwise it returns a consistent layout of exactly the
sizes and chunk size it was given. -/
@[step]
theorem artifact.layout.Layout.new.spec (sizes : Slice U64) (chunk_size : U32) :
    artifact.layout.Layout.new sizes chunk_size
    ⦃ r =>
      match r with
      | none => chunk_size.val = 0 ∨ U64.max < bytesBefore sizes.val sizes.val.length
      | some l => l.chunk_size = chunk_size ∧ l.sizes.val = sizes.val ∧ l.WellFormed ⦄ := by
  simp only [artifact.layout.Layout.new]
  split
  · rename_i hzero
    simp only [WP.spec_ok]
    left
    scalar_tac
  · rename_i hnonzero
    have hc : 0 < chunk_size.val := by scalar_tac
    have hstart : layoutInv sizes chunk_size
        (alloc.vec.Vec.with_capacity U64 (Slice.len sizes), 0#u64, 0#u64, 0#usize) := by
      simp [layoutInv, chunksBefore, bytesBefore, sumBefore, alloc.vec.Vec.with_capacity,
        alloc.vec.Vec.new]
    apply WP.spec_mono
      (artifact.layout.Layout.new_loop.spec sizes chunk_size _ _ _ _ hc hstart)
    intro r hr
    cases r with
    | none => exact Or.inr hr
    | some l => exact hr

/-- `file_count` is the number of files. -/
@[step]
theorem artifact.layout.Layout.file_count.spec (l : Layout) :
    artifact.layout.Layout.file_count l ⦃ r => r.val = l.sizes.val.length ⦄ := by
  unfold artifact.layout.Layout.file_count
  simp only [WP.spec_ok]
  scalar_tac

/-- `chunk_count` is the number of chunks in all the files. -/
@[step]
theorem artifact.layout.Layout.impl.chunk_count.spec (l : Layout) (hl : l.WellFormed) :
    artifact.layout.Layout.impl.chunk_count l
    ⦃ r => r.val = chunksBefore l.sizes.val l.chunk_size.val l.sizes.val.length ⦄ := by
  unfold artifact.layout.Layout.impl.chunk_count
  simp only [WP.spec_ok]
  exact hl.2.2.2.1

/-- `total_size` is the sum of the sizes of the files. -/
@[step]
theorem artifact.layout.Layout.impl.total_size.spec (l : Layout) (hl : l.WellFormed) :
    artifact.layout.Layout.impl.total_size l
    ⦃ r => r.val = bytesBefore l.sizes.val l.sizes.val.length ⦄ := by
  unfold artifact.layout.Layout.impl.total_size
  simp only [WP.spec_ok]
  exact hl.2.2.2.2

/-- `file_chunks` returns the number of the file's first chunk and the
number after its last. -/
@[step]
theorem artifact.layout.Layout.file_chunks.spec (l : Layout) (file : Usize)
    (hl : l.WellFormed) (hfile : file.val < l.sizes.val.length) :
    artifact.layout.Layout.file_chunks l file
    ⦃ start stop =>
      start.val = chunksBefore l.sizes.val l.chunk_size.val file.val ∧
      stop.val = chunksBefore l.sizes.val l.chunk_size.val (file.val + 1) ⦄ := by
  unfold artifact.layout.Layout.file_chunks
  simp only [artifact.layout.Layout.WellFormed] at hl
  obtain ⟨ hc, hfiles, hfirst, hcount, htotal ⟩ := hl
  have hinfirst : file.val < l.first_chunk.val.length := by omega
  step as ⟨ start, hstart ⟩
  have hstartval := val_of_getElem? hinfirst hstart (hfirst file.val hfile)
  step as ⟨ size, hsize ⟩
  have hget : l.sizes.val[file.val]? = some size := by
    rw [hsize]
    exact List.getElem?_eq_getElem hfile
  have hnext := chunksBefore_succ l.sizes.val l.chunk_size.val file.val size hget
  have hmono := chunksBefore_mono l.sizes.val l.chunk_size.val (n := file.val + 1)
    (m := l.sizes.val.length) (by omega)
  step as ⟨ wide, hwide ⟩
  step as ⟨ count, hcountval ⟩
  rw [hwide] at hcountval
  step as ⟨ stop, hstop ⟩
  exact ⟨ hstartval, by omega ⟩

/-- What is true each time round the search in `locate`: the file sought is
at or after `low` and before `high`. -/
def locateInv (firsts : alloc.vec.Vec U64) (index : U64) (p : Usize × Usize) : Prop :=
  p.1.val < p.2.val ∧ p.2.val ≤ firsts.val.length ∧
  (∃ first, firsts.val[p.1.val]? = some first ∧ first.val ≤ index.val) ∧
  (p.2.val < firsts.val.length →
    ∃ first, firsts.val[p.2.val]? = some first ∧ index.val < first.val)

/-- The search in `locate` ends on a file whose first chunk is at or before
the index, and whose successor's first chunk, if it has a successor, is
after it. -/
theorem artifact.layout.Layout.locate_loop.spec (firsts : alloc.vec.Vec U64) (index : U64)
    (low high : Usize) (hinv : locateInv firsts index (low, high)) :
    artifact.layout.Layout.locate_loop firsts index low high
    ⦃ file =>
      file.val < firsts.val.length ∧
      (∃ first, firsts.val[file.val]? = some first ∧ first.val ≤ index.val) ∧
      (file.val + 1 < firsts.val.length →
        ∃ first, firsts.val[file.val + 1]? = some first ∧ index.val < first.val) ⦄ := by
  unfold artifact.layout.Layout.locate_loop
  apply loop.spec_decr_nat
    (measure := fun (p : Usize × Usize) => p.2.val - p.1.val)
    (inv := locateInv firsts index)
  · intro ⟨ lo, hi ⟩ hstate
    simp only [locateInv] at hstate
    obtain ⟨ hlt, hle, hlow, hhigh ⟩ := hstate
    simp only [artifact.layout.Layout.locate_loop.body]
    step as ⟨ gap, hgap ⟩
    split
    · rename_i hwide
      step as ⟨ half, hhalf ⟩
      step as ⟨ middle, hmiddle ⟩
      have hbetween : lo.val < middle.val ∧ middle.val < hi.val := by scalar_tac
      step as ⟨ first, hfirst ⟩
      have hget : firsts.val[middle.val]? = some first := by
        rw [hfirst]
        exact List.getElem?_eq_getElem (by omega)
      split
      · rename_i hbelow
        simp only [WP.spec_ok]
        exact ⟨ ⟨ hbetween.2, hle, ⟨ first, hget, by scalar_tac ⟩, hhigh ⟩, by scalar_tac ⟩
      · rename_i habove
        simp only [WP.spec_ok]
        refine ⟨ ⟨ hbetween.1, ?_, hlow, fun _ => ⟨ first, hget, by scalar_tac ⟩ ⟩,
          by scalar_tac ⟩
        dsimp only
        omega
    · rename_i hnarrow
      have hnext : hi.val = lo.val + 1 := by scalar_tac
      simp only [WP.spec_ok]
      refine ⟨ by omega, hlow, ?_ ⟩
      intro hmore
      rw [← hnext]
      exact hhigh (by omega)
  · exact hinv

/-- What `locate` says about chunk `index`: the file it is in, that it is
chunk number `index - (chunks before that file)` of that file, where it
starts, and how long it is. -/
def artifact.layout.Layout.Locates (l : Layout) (index : Nat) (chunk : ChunkLocation) :
    Prop :=
  ∃ size : U64, l.sizes.val[chunk.file.val]? = some size ∧
    chunksBefore l.sizes.val l.chunk_size.val chunk.file.val ≤ index ∧
    index < chunksBefore l.sizes.val l.chunk_size.val (chunk.file.val + 1) ∧
    chunk.offset.val =
      (index - chunksBefore l.sizes.val l.chunk_size.val chunk.file.val) * l.chunk_size.val ∧
    chunk.len.val = chunkLen size.val l.chunk_size.val
      (index - chunksBefore l.sizes.val l.chunk_size.val chunk.file.val)

/-- `locate` returns nothing exactly when the index is past the last chunk.
Otherwise it names the file whose range of chunks holds the index, an offset
on a chunk boundary, and the right length. -/
@[step]
theorem artifact.layout.Layout.locate.spec (l : Layout) (index : U64) (hl : l.WellFormed) :
    artifact.layout.Layout.locate l index
    ⦃ r =>
      match r with
      | none => l.chunk_count.val ≤ index.val
      | some chunk => index.val < l.chunk_count.val ∧ l.Locates index.val chunk ⦄ := by
  simp only [artifact.layout.Layout.locate]
  simp only [artifact.layout.Layout.WellFormed] at hl
  obtain ⟨ hc, hfiles, hfirst, hcount, htotal ⟩ := hl
  split
  · rename_i hout
    simp only [WP.spec_ok]
    scalar_tac
  · rename_i hin
    have hindex : index.val < l.chunk_count.val := by scalar_tac
    -- There is a chunk, so there is a file.
    have hsome : 0 < l.sizes.val.length := by
      by_cases hnone : l.sizes.val.length = 0
      · rw [hcount, hnone] at hindex
        simp [chunksBefore, sumBefore] at hindex
      · omega
    have hstart : locateInv l.first_chunk index
        (0#usize, alloc.vec.Vec.len l.first_chunk) := by
      obtain ⟨ first, hget, hval ⟩ := hfirst 0 hsome
      simp only [locateInv]
      refine ⟨ by scalar_tac, by scalar_tac, ⟨ first, by simpa using hget, ?_ ⟩, ?_ ⟩
      · rw [hval]
        simp [chunksBefore, sumBefore]
      · intro hpast
        scalar_tac
    step with artifact.layout.Layout.locate_loop.spec l.first_chunk index 0#usize _ hstart
      as ⟨ file, hfile, below, hbelowget, hbelowle, habove ⟩
    have hfilelt : file.val < l.sizes.val.length := by omega
    obtain ⟨ size, hsizeget ⟩ : ∃ size, l.sizes.val[file.val]? = some size :=
      ⟨ _, List.getElem?_eq_getElem hfilelt ⟩
    have hlo : chunksBefore l.sizes.val l.chunk_size.val file.val ≤ index.val := by
      obtain ⟨ y, hy, hyval ⟩ := hfirst file.val hfilelt
      rw [hbelowget] at hy
      have hxy : below = y := Option.some.inj hy
      rw [← hyval, ← hxy]
      exact hbelowle
    have hhi : index.val < chunksBefore l.sizes.val l.chunk_size.val (file.val + 1) := by
      by_cases hlast : file.val + 1 < l.sizes.val.length
      · obtain ⟨ x, hx, hxlt ⟩ := habove (by omega)
        obtain ⟨ y, hy, hyval ⟩ := hfirst (file.val + 1) hlast
        rw [hx] at hy
        have hxy : x = y := Option.some.inj hy
        rw [← hyval, ← hxy]
        exact hxlt
      · have hend : file.val + 1 = l.sizes.val.length := by omega
        rw [hend, ← hcount]
        exact hindex
    have hnext := chunksBefore_succ l.sizes.val l.chunk_size.val file.val size hsizeget
    -- The chunk starts inside the file, so nothing below can overflow.
    have hinside := (lt_chunksIn_iff size.val l.chunk_size.val
      (index.val - chunksBefore l.sizes.val l.chunk_size.val file.val) hc).mp (by omega)
    step as ⟨ wide, hwide ⟩
    step as ⟨ first, hfirstget ⟩
    have hfirstval := val_of_getElem? (by omega) hfirstget (hfirst file.val hfilelt)
    step as ⟨ within, hwithin ⟩
    have hfits : within.val * wide.val < size.val := by
      rw [hwithin, hfirstval, hwide]
      exact hinside
    step as ⟨ offset, hoffset ⟩
    have hoffsetval : offset.val =
        (index.val - chunksBefore l.sizes.val l.chunk_size.val file.val) * l.chunk_size.val := by
      rw [hoffset, hwithin, hfirstval, hwide]
    step as ⟨ size', hsize' ⟩
    have hsame : size' = size := by
      have hread := hsizeget
      rw [List.getElem?_eq_getElem hfilelt] at hread
      rw [hsize']
      exact Option.some.inj hread
    rw [hsame]
    step as ⟨ remaining, hremaining ⟩
    split
    · rename_i hshort
      step as ⟨ len, hlen, hclamped ⟩
      simp only [artifact.layout.Layout.Locates, chunkLen]
      refine ⟨ hindex, size, hsizeget, hlo, hhi, hoffsetval, ?_ ⟩
      rw [← hoffsetval, hlen (by scalar_tac)]
      scalar_tac
    · rename_i hfull
      simp only [WP.spec_ok, artifact.layout.Layout.Locates, chunkLen]
      refine ⟨ hindex, size, hsizeget, hlo, hhi, hoffsetval, ?_ ⟩
      rw [← hoffsetval]
      scalar_tac

/-- A located chunk is not empty, is no longer than the chunk size, lies
wholly inside its file, and is a whole chunk unless it reaches the end of
the file. -/
theorem artifact.layout.Layout.Locates.inside (l : Layout) (index : Nat)
    (chunk : ChunkLocation) (hl : l.WellFormed) (h : l.Locates index chunk) :
    ∃ size : U64, l.sizes.val[chunk.file.val]? = some size ∧
      0 < chunk.len.val ∧ chunk.len.val ≤ l.chunk_size.val ∧
      chunk.offset.val + chunk.len.val ≤ size.val ∧
      (chunk.len.val = l.chunk_size.val ∨ chunk.offset.val + chunk.len.val = size.val) := by
  simp only [artifact.layout.Layout.WellFormed] at hl
  simp only [artifact.layout.Layout.Locates, chunkLen] at h
  obtain ⟨ hc, -, -, -, - ⟩ := hl
  obtain ⟨ size, hsize, hlo, hhi, hoffset, hlen ⟩ := h
  have hnext := chunksBefore_succ l.sizes.val l.chunk_size.val chunk.file.val size hsize
  have hinside := (lt_chunksIn_iff size.val l.chunk_size.val
    (index - chunksBefore l.sizes.val l.chunk_size.val chunk.file.val) hc).mp (by omega)
  rw [← hoffset] at hinside hlen
  exact ⟨ size, hsize, by omega, by omega, by omega, by omega ⟩

/-- `locate` and `file_chunks` agree: the file `locate` names is the one
whose range of chunks holds the index. -/
theorem artifact.layout.Layout.locate_agrees_with_file_chunks (l : Layout) (index : U64)
    (hl : l.WellFormed) :
    artifact.layout.Layout.locate l index
    ⦃ r =>
      ∀ chunk, r = some chunk →
        artifact.layout.Layout.file_chunks l chunk.file
        ⦃ start stop => start.val ≤ index.val ∧ index.val < stop.val ⦄ ⦄ := by
  apply WP.spec_mono (artifact.layout.Layout.locate.spec l index hl)
  intro r hr chunk hsome
  subst hsome
  obtain ⟨ -, hlocates ⟩ : index.val < l.chunk_count.val ∧ l.Locates index.val chunk := hr
  simp only [artifact.layout.Layout.Locates] at hlocates
  obtain ⟨ size, hsize, hlo, hhi, -, - ⟩ := hlocates
  apply WP.spec_mono
    (artifact.layout.Layout.file_chunks.spec l chunk.file hl (lt_length_of_getElem? hsize))
  intro p hp
  obtain ⟨ start, stop ⟩ := p
  simp only [WP.uncurry'] at hp ⊢
  omega

/-- Every byte of every file lies in exactly one chunk. Byte `b` of file `f`
is in chunk number `(chunks before f) + b / chunk_size`. That chunk exists,
`locate` puts it over the byte, and `locate` puts no other chunk there. -/
theorem artifact.layout.Layout.every_byte_in_one_chunk (l : Layout) (hl : l.WellFormed)
    (f b : Nat) (size : U64) (hsize : l.sizes.val[f]? = some size) (hb : b < size.val)
    (index : U64) :
    chunksBefore l.sizes.val l.chunk_size.val f + b / l.chunk_size.val < l.chunk_count.val ∧
    artifact.layout.Layout.locate l index
    ⦃ r =>
      (∃ chunk, r = some chunk ∧ chunk.file.val = f ∧
        chunk.offset.val ≤ b ∧ b < chunk.offset.val + chunk.len.val) ↔
      index.val = chunksBefore l.sizes.val l.chunk_size.val f + b / l.chunk_size.val ⦄ := by
  have hwf := hl
  simp only [artifact.layout.Layout.WellFormed] at hl
  obtain ⟨ hc, -, -, hcount, - ⟩ := hl
  have hf := lt_length_of_getElem? hsize
  have hnext := chunksBefore_succ l.sizes.val l.chunk_size.val f size hsize
  have hmono := chunksBefore_mono l.sizes.val l.chunk_size.val (n := f + 1)
    (m := l.sizes.val.length) (by omega)
  have hbounds := div_mul_bounds b l.chunk_size.val hc
  -- The chunk of the file that holds the byte exists.
  have hexists := (lt_chunksIn_iff size.val l.chunk_size.val (b / l.chunk_size.val) hc).mpr
    (by omega)
  -- From here on the byte's chunk within its file is just a number.
  generalize b / l.chunk_size.val = within at hbounds hexists ⊢
  have hvalid :
      chunksBefore l.sizes.val l.chunk_size.val f + within < l.chunk_count.val := by
    omega
  refine ⟨ hvalid, ?_ ⟩
  apply WP.spec_mono (artifact.layout.Layout.locate.spec l index hwf)
  intro r hr
  cases r with
  | none =>
    have hout : l.chunk_count.val ≤ index.val := hr
    constructor
    · intro ⟨ chunk, hnone, _ ⟩
      exact absurd hnone (by simp)
    · intro hat
      exfalso
      omega
  | some chunk =>
    obtain ⟨ -, hlocates ⟩ : index.val < l.chunk_count.val ∧ l.Locates index.val chunk := hr
    simp only [artifact.layout.Layout.Locates, chunkLen] at hlocates
    obtain ⟨ size', hsize', hlo, hhi, hoffset, hlen ⟩ := hlocates
    constructor
    · intro ⟨ chunk', hsome, hfile, hfrom, hto ⟩
      have hchunk : chunk = chunk' := Option.some.inj hsome
      subst hchunk
      subst hfile
      rw [hsize] at hsize'
      have hsame : size = size' := Option.some.inj hsize'
      subst hsame
      -- The chunk covers the byte, so it is the byte's chunk within the file.
      have hk := chunk_of_byte_unique b l.chunk_size.val within
        (index.val - chunksBefore l.sizes.val l.chunk_size.val chunk.file.val)
        hbounds.1 hbounds.2 (by omega) (by omega)
      omega
    · intro hat
      -- Two files cannot both hold the same chunk.
      have hfile : chunk.file.val = f :=
        file_of_chunk_unique l.sizes.val l.chunk_size.val chunk.file.val f index.val
          hlo hhi (by omega) (by omega)
      subst hfile
      rw [hsize] at hsize'
      have hsame : size = size' := Option.some.inj hsize'
      subst hsame
      have hk : index.val - chunksBefore l.sizes.val l.chunk_size.val chunk.file.val =
          within := by omega
      rw [hk] at hoffset hlen
      exact ⟨ chunk, rfl, rfl, by omega, by omega ⟩

/-- The chunk lengths add up to the total size: each file's chunks add up to
that file's size, and the total size is the sum of the files. -/
theorem artifact.layout.Layout.chunk_lengths_add_up (l : Layout) (hl : l.WellFormed) :
    (l.sizes.val.map (fun size =>
      ((List.range (chunksIn size.val l.chunk_size.val)).map
        (chunkLen size.val l.chunk_size.val)).sum)).sum = l.total_size.val := by
  simp only [artifact.layout.Layout.WellFormed] at hl
  obtain ⟨ hc, -, -, -, htotal ⟩ := hl
  rw [htotal]
  simp only [fun size => file_chunkLens_sum size l.chunk_size.val hc, bytesBefore, sumBefore,
    List.take_length]

end model
