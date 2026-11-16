-- What is proven about `model::schedule`.
--
-- A schedule records, for each chunk of a transfer, whether it is missing,
-- being fetched or done, and how many are not done yet.
--
--   * `complete` and `release` change a chunk only if it is being fetched,
--     and change no other chunk.
--   * The count of chunks not yet done stays right through every operation.
--   * `claim` hands out only a chunk that is missing and that the supplier
--     holds, marks exactly that chunk as being fetched, and otherwise
--     changes nothing.
--   * No operation can fail: none overflows, divides by zero or reads past
--     the end of the list.
module
public import Aeneas
public import Model.Bitfield
@[expose] public section
open Aeneas Aeneas.Std Result

namespace model

open schedule

deriving instance DecidableEq for schedule.ChunkState

/-- How many chunks are not done yet. -/
def notDone (chunks : List ChunkState) : Nat :=
  chunks.countP (fun state => decide (state ≠ ChunkState.Done))

/-- The schedule's own count agrees with its list of chunks. -/
def schedule.Schedule.Counted (s : Schedule) : Prop :=
  s.remaining.val = notDone s.chunks.val

/-- Comparing two states never fails and says whether they are the same. -/
@[step]
theorem schedule.ChunkState.eq.spec (a b : ChunkState) :
    schedule.ChunkState.Insts.CoreCmpPartialEqChunkState.eq a b
    ⦃ r => r = decide (a = b) ⦄ := by
  unfold schedule.ChunkState.Insts.CoreCmpPartialEqChunkState.eq
  cases a <;> cases b <;> simp [read_discriminant]

/-- `settle` moves a chunk that is being fetched to the given state and
leaves every other chunk alone. For any other chunk, and for an index past
the end, it changes nothing and says so. -/
@[step]
theorem schedule.Schedule.settle.spec (s : Schedule) (index : U64) (state : ChunkState) :
    schedule.Schedule.settle s index state
    ⦃ settled s' =>
      if s.chunks.val[index.val]? = some ChunkState.InFlight then
        settled = true ∧ s'.chunks.val = s.chunks.val.set index.val state ∧
        s'.remaining = s.remaining
      else settled = false ∧ s' = s ⦄ := by
  unfold schedule.Schedule.settle
  step as ⟨ slot, hfits, hover ⟩
  -- A slot inside the list is the index itself: clamping only happens to
  -- indices far past any list.
  have hslot : slot.val < s.chunks.val.length → slot.val = index.val := by
    intro hin
    by_cases h : index.val ≤ Usize.max
    · exact hfits h
    · have := hover (by omega)
      scalar_tac
  have hpast : ¬ slot.val < s.chunks.val.length → s.chunks.val.length ≤ index.val := by
    intro hout
    by_cases h : index.val ≤ Usize.max
    · have := hfits h
      omega
    · scalar_tac
  split
  · rename_i hin
    have heq := hslot (by scalar_tac)
    have hidx : index.val < s.chunks.val.length := by scalar_tac
    rw [List.getElem?_eq_getElem hidx]
    step as ⟨ cs, hcs ⟩
    step as ⟨ b, hb ⟩
    split
    · step as ⟨ old, back, hold, hback ⟩
      simp_all
    · simp_all
  · rename_i hout
    have := hpast (by scalar_tac)
    simp_all

/-- Changing one chunk's state changes the count of chunks not yet done by
the difference between the old state and the new one. -/
theorem notDone_set (chunks : List ChunkState) (i : Nat) (old new : ChunkState)
    (h : chunks[i]? = some old) :
    notDone (chunks.set i new) + (if old = ChunkState.Done then 0 else 1) =
      notDone chunks + (if new = ChunkState.Done then 0 else 1) := by
  induction chunks generalizing i with
  | nil => simp at h
  | cons head tail ih =>
    cases i with
    | zero =>
      simp at h
      subst h
      cases head <;> cases new <;> simp [notDone]
    | succ j =>
      simp at h
      have := ih j h
      simp [notDone, List.countP_cons] at *
      omega

/-- `release` puts a chunk that is being fetched back to missing, and
changes nothing else. The count stays right. -/
@[step]
theorem schedule.Schedule.release.spec (s : Schedule) (index : U64)
    (hs : s.Counted) :
    schedule.Schedule.release s index
    ⦃ released s' =>
      s'.Counted ∧
      if s.chunks.val[index.val]? = some ChunkState.InFlight then
        released = true ∧
        s'.chunks.val = s.chunks.val.set index.val ChunkState.Missing ∧
        s'.remaining = s.remaining
      else released = false ∧ s' = s ⦄ := by
  unfold schedule.Schedule.release
  step as ⟨ released, s', h ⟩
  split at h
  · rename_i hflight
    obtain ⟨ hreleased, hchunks, hremaining ⟩ := h
    have := notDone_set s.chunks.val index.val ChunkState.InFlight ChunkState.Missing hflight
    simp_all [Schedule.Counted]
  · simp_all

/-- `complete` marks a chunk that is being fetched as done, and changes
nothing else. The count goes down by exactly one, and stays right. -/
@[step]
theorem schedule.Schedule.complete.spec (s : Schedule) (index : U64)
    (hs : s.Counted) :
    schedule.Schedule.complete s index
    ⦃ completed s' =>
      s'.Counted ∧
      if s.chunks.val[index.val]? = some ChunkState.InFlight then
        completed = true ∧
        s'.chunks.val = s.chunks.val.set index.val ChunkState.Done ∧
        s'.remaining.val + 1 = s.remaining.val
      else completed = false ∧ s' = s ⦄ := by
  unfold schedule.Schedule.complete
  step as ⟨ settled, s1, h ⟩
  split at h
  · rename_i hflight
    obtain ⟨ hsettled, hchunks, hremaining ⟩ := h
    have hcount := notDone_set s.chunks.val index.val ChunkState.InFlight ChunkState.Done hflight
    simp [Schedule.Counted] at hs
    simp [hsettled]
    step as ⟨ remaining, hsub ⟩
    simp_all [Schedule.Counted]
    omega
  · simp_all

open artifact.bitfield in
/-- What a chunk's state is at the start: done if it is already held, and
missing otherwise. -/
def initialState (held : Bitfield) (i : Nat) : ChunkState :=
  if held.bit i then ChunkState.Done else ChunkState.Missing

open artifact.bitfield in
/-- What is true each time round the loop in `new`: the chunks so far are
the right ones, and the count agrees with them. -/
def newInv (held : Bitfield) (total : U64)
    (p : alloc.vec.Vec ChunkState × U64 × U64) : Prop :=
  p.2.2.val ≤ total.val ∧ p.1.val.length = p.2.2.val ∧
  (∀ i, i < p.2.2.val → p.1.val[i]? = some (initialState held i)) ∧
  p.2.1.val = notDone p.1.val

open artifact.bitfield in
theorem schedule.Schedule.new_loop.spec (held : Bitfield) (total : U64)
    (chunks : alloc.vec.Vec ChunkState) (remaining index : U64)
    (hsized : held.Sized) (hfits : total.val ≤ Usize.max)
    (hinv : newInv held total (chunks, remaining, index)) :
    schedule.Schedule.new_loop held total chunks remaining index
    ⦃ chunks' remaining' =>
      chunks'.val.length = total.val ∧
      (∀ i, i < total.val → chunks'.val[i]? = some (initialState held i)) ∧
      remaining'.val = notDone chunks'.val ⦄ := by
  unfold schedule.Schedule.new_loop
  apply loop.spec_decr_nat
    (measure := fun (p : alloc.vec.Vec ChunkState × U64 × U64) => total.val - p.2.2.val)
    (inv := newInv held total)
  · intro ⟨ c, r, i ⟩ hc
    simp only [newInv] at hc
    obtain ⟨ hle, hlen, hstates, hcount ⟩ := hc
    simp only [schedule.Schedule.new_loop.body]
    split
    · rename_i hlt
      have hlt' : i.val < total.val := by scalar_tac
      step as ⟨ b, hb ⟩
      split
      · rename_i hheld
        step as ⟨ c2, hc2 ⟩
        step as ⟨ i1, hi1 ⟩
        refine ⟨ ⟨ by scalar_tac, by simp [hc2, hlen, hi1], ?_, ?_ ⟩, by scalar_tac ⟩
        · intro j hj
          simp only [hc2]
          by_cases hprev : j < i.val
          · rw [List.getElem?_append_left (by omega)]
            exact hstates j hprev
          · have hji : j = i.val := by scalar_tac
            subst hji
            have hbit : held.bit i.val = true := by rw [← hb]; exact hheld
            have hlast : (c.val ++ [ChunkState.Done])[i.val]? = some ChunkState.Done := by
              rw [← hlen]
              simp
            rw [hlast]
            simp [initialState, hbit]
        · simp [hc2, notDone, List.countP_append] at *
          exact hcount
      · rename_i hnotheld
        have hrle : r.val ≤ i.val := by
          rw [hcount, ← hlen]
          exact List.countP_le_length
        step as ⟨ c2, hc2 ⟩
        step as ⟨ r2, hr2 ⟩
        step as ⟨ i1, hi1 ⟩
        refine ⟨ ⟨ by scalar_tac, by simp [hc2, hlen, hi1], ?_, ?_ ⟩, by scalar_tac ⟩
        · intro j hj
          simp only [hc2]
          by_cases hprev : j < i.val
          · rw [List.getElem?_append_left (by omega)]
            exact hstates j hprev
          · have hji : j = i.val := by scalar_tac
            subst hji
            have hbit : held.bit i.val = false := by
              rw [← hb]
              simpa using hnotheld
            have hlast : (c.val ++ [ChunkState.Missing])[i.val]? = some ChunkState.Missing := by
              rw [← hlen]
              simp
            rw [hlast]
            simp [initialState, hbit]
        · simp [hc2, notDone, List.countP_append, hr2] at *
          exact hcount
    · rename_i hstop
      have hfull : i.val = total.val := by scalar_tac
      simp only [WP.spec_ok, WP.uncurry']
      refine ⟨ by omega, ?_, hcount ⟩
      intro j hj
      exact hstates j (by omega)
  · exact hinv

open artifact.bitfield in
/-- `new` gives a schedule with one state per chunk: done for each chunk
already held, missing for the rest, and nothing being fetched. Its count is
right. -/
@[step]
theorem schedule.Schedule.new.spec (held : Bitfield)
    (hsized : held.Sized) (hfits : held.len.val ≤ Usize.max) :
    schedule.Schedule.new held
    ⦃ s =>
      s.Counted ∧ s.chunks.val.length = held.len.val ∧
      ∀ i, i < held.len.val → s.chunks.val[i]? = some (initialState held i) ⦄ := by
  unfold schedule.Schedule.new
  simp only [artifact.bitfield.Bitfield.impl.len]
  step as ⟨ capacity, _, _ ⟩
  have hstart : newInv held held.len
      (alloc.vec.Vec.with_capacity ChunkState capacity, 0#u64, 0#u64) := by
    simp [newInv, notDone, alloc.vec.Vec.with_capacity, alloc.vec.Vec.new]
  step with schedule.Schedule.new_loop.spec held held.len _ _ _ hsized hfits hstart
    as ⟨ chunks, remaining, hlength, hstates, hcount ⟩
  exact ⟨ hcount, hlength, hstates ⟩

/-- The number of chunks, as the schedule reports it. -/
@[step]
theorem schedule.Schedule.len.spec (s : Schedule) :
    schedule.Schedule.len s ⦃ r => r.val = s.chunks.val.length ⦄ := by
  unfold schedule.Schedule.len
  simp only [WP.spec_ok]
  have hbound : s.chunks.len.val ≤ U64.max := by
    have := usize_max_le_u64_max
    scalar_tac
  have hcast := UScalar.cast_inBounds_spec (src_ty := .Usize) (tgt_ty := .U64) s.chunks.len
    (by simpa [UScalar.max, U64.max] using hbound)
  simp only [lift, WP.spec_ok] at hcast
  simp [hcast]

/-- The position `offset` places after `start`, going round a ring of
`total` positions. -/
def ringAt (total start offset : Nat) : Nat :=
  if start + offset < total then start + offset else start + offset - total

/-- Going all the way round a ring passes every position. -/
theorem ringAt_reaches (total start i : Nat) (hstart : start < total) (hi : i < total) :
    ∃ k, k < total ∧ ringAt total start k = i := by
  by_cases h : start ≤ i
  · exact ⟨ i - start, by omega, by simp only [ringAt]; split <;> omega ⟩
  · exact ⟨ i + total - start, by omega, by simp only [ringAt]; split <;> omega ⟩

open artifact.bitfield in
/-- A chunk that `claim` may hand out: it is missing, and the supplier
holds it. -/
def claimable (chunks : List ChunkState) (available : Bitfield) (i : Nat) : Prop :=
  chunks[i]? = some ChunkState.Missing ∧ available.bit i = true

open artifact.bitfield in
/-- What is true each time round the search loop in `claim`: the position
is where it should be on the ring, and nothing looked at so far could be
claimed. -/
def claimInv (chunks : List ChunkState) (available : Bitfield) (start : Nat)
    (p : U64 × U64) : Prop :=
  p.2.val ≤ chunks.length ∧
  p.1.val = ringAt chunks.length start p.2.val ∧
  ∀ k, k < p.2.val → ¬ claimable chunks available (ringAt chunks.length start k)

open artifact.bitfield in
theorem schedule.Schedule.claim_loop.spec (v : alloc.vec.Vec ChunkState)
    (available : Bitfield) (cursor total index step : U64) (start : Nat)
    (hsized : available.Sized) (htotal : total.val = v.val.length)
    (hstart : start < v.val.length)
    (hinv : claimInv v.val available start (index, step)) :
    schedule.Schedule.claim_loop v available cursor total index step
    ⦃ found v' cursor' =>
      match found with
      | some chosen =>
        claimable v.val available chosen.val ∧
        v'.val = v.val.set chosen.val ChunkState.InFlight ∧
        cursor'.val < v.val.length
      | none =>
        v' = v ∧ cursor' = cursor ∧ ∀ i, ¬ claimable v.val available i ⦄ := by
  unfold schedule.Schedule.claim_loop
  apply loop.spec_decr_nat
    (measure := fun (p : U64 × U64) => total.val - p.2.val)
    (inv := claimInv v.val available start)
  · intro ⟨ at_, looked ⟩ hc
    simp only [claimInv] at hc
    obtain ⟨ hle, hat, hnone ⟩ := hc
    simp only [schedule.Schedule.claim_loop.body]
    split
    · rename_i hlt
      have hlooked : looked.val < v.val.length := by scalar_tac
      have hatlt : at_.val < v.val.length := by
        rw [hat]
        simp only [ringAt]
        split <;> omega
      step as ⟨ slot, hfits, hover ⟩
      have hslot : slot.val = at_.val := hfits (by scalar_tac)
      step as ⟨ after, hafter ⟩
      -- Where the search goes next: one place on, wrapping at the end.
      have hwrap : (if after = total then ok 0#u64 else ok after : Result U64) =
          ok (if after = total then 0#u64 else after) := by
        split <;> rfl
      simp only [hwrap]
      generalize hnext : (if after = total then 0#u64 else after) = next
      have hring : ringAt v.val.length start (looked.val + 1) =
          (if at_.val + 1 = v.val.length then 0 else at_.val + 1) := by
        rw [hat]
        unfold ringAt
        split_ifs <;> omega
      have hnextval : next.val = ringAt v.val.length start (looked.val + 1) := by
        rw [hring, ← hnext]
        have hend_iff : after = total ↔ at_.val + 1 = v.val.length := by
          rw [UScalar.eq_equiv]
          omega
        by_cases hend : after = total
        · have hlen := hend_iff.mp hend
          simp only [hend, hlen, ↓reduceIte]
          rfl
        · have hlen : ¬ at_.val + 1 = v.val.length := fun h => hend (hend_iff.mpr h)
          simp only [hend, hlen, ↓reduceIte]
          exact hafter
      have hnextlt : next.val < v.val.length := by
        rw [hnextval]
        simp only [ringAt]
        split <;> omega
      step as ⟨ state, hstate ⟩
      step as ⟨ missing, hmissing ⟩
      -- Looking at this position and not claiming it keeps the invariant.
      have hmoveon : ¬ claimable v.val available at_.val →
          ∀ looked' : U64, looked'.val = looked.val + 1 →
          claimInv v.val available start (next, looked') ∧
            total.val - looked'.val < total.val - looked.val := by
        intro hnot looked' hlooked'
        refine ⟨ ⟨ by scalar_tac, by rw [hnextval, hlooked'], ?_ ⟩, by scalar_tac ⟩
        intro k hk
        by_cases hprev : k < looked.val
        · exact hnone k hprev
        · have hkeq : k = looked.val := by scalar_tac
          rw [hkeq, ← hat]
          exact hnot
      split
      · rename_i hismissing
        step as ⟨ offered, hoffered ⟩
        split
        · rename_i hisoffered
          step as ⟨ old, back, hold, hback ⟩
          have hget : v.val[at_.val]? = some ChunkState.Missing := by
            rw [List.getElem?_eq_getElem hatlt]
            simp_all
          refine ⟨ ⟨ hget, by rw [← hoffered]; exact hisoffered ⟩, ?_, hnextlt ⟩
          simp [hback, hslot]
        · rename_i hnotoffered
          step as ⟨ looked', hlooked' ⟩
          refine hmoveon ?_ looked' hlooked'
          intro hclaim
          exact hnotoffered (by rw [hoffered]; exact hclaim.2)
      · rename_i hnotmissing
        step as ⟨ looked', hlooked' ⟩
        refine hmoveon ?_ looked' hlooked'
        intro hclaim
        have hget := hclaim.1
        rw [List.getElem?_eq_getElem hatlt] at hget
        simp_all
    · rename_i hstop
      have hall : looked.val = v.val.length := by scalar_tac
      simp only [WP.spec_ok, WP.uncurry']
      refine ⟨ trivial, trivial, ?_ ⟩
      intro i hclaim
      by_cases hi : i < v.val.length
      · obtain ⟨ k, hk, hreach ⟩ := ringAt_reaches v.val.length start i hstart hi
        exact hnone k (by omega) (by rw [hreach]; exact hclaim)
      · have hpast : v.val[i]? = none := List.getElem?_eq_none (by omega)
        have hmissing := hclaim.1
        rw [hpast] at hmissing
        exact absurd hmissing (by simp)
  · exact hinv

/-- When no chunk is left to do, none is missing. -/
theorem none_missing_when_all_done (chunks : List ChunkState) (h : notDone chunks = 0)
    (i : Nat) : chunks[i]? ≠ some ChunkState.Missing := by
  intro hget
  have hmem : ChunkState.Missing ∈ chunks := List.mem_of_getElem? hget
  simp only [notDone, List.countP_eq_zero] at h
  exact absurd (h ChunkState.Missing hmem) (by simp)

open artifact.bitfield in
/-- `claim` hands out a chunk only if that chunk is missing and the
supplier holds it. It then marks exactly that chunk as being fetched and
moves the cursor to a valid position. It returns nothing only when no chunk
at all could be claimed, and then changes nothing. The count stays right
either way, and the call cannot fail. -/
@[step]
theorem schedule.Schedule.claim.spec (s : Schedule) (available : Bitfield) (cursor : U64)
    (hs : s.Counted) (hsized : available.Sized) :
    schedule.Schedule.claim s available cursor
    ⦃ found s' cursor' =>
      s'.Counted ∧
      match found with
      | some chosen =>
        claimable s.chunks.val available chosen.val ∧
        s'.chunks.val = s.chunks.val.set chosen.val ChunkState.InFlight ∧
        s'.remaining = s.remaining ∧ cursor'.val < s.chunks.val.length
      | none =>
        s' = s ∧ cursor' = cursor ∧ ∀ i, ¬ claimable s.chunks.val available i ⦄ := by
  unfold schedule.Schedule.claim
  step as ⟨ total, htotal ⟩
  split
  · rename_i hzero
    have hnone : notDone s.chunks.val = 0 := by
      simp only [schedule.Schedule.Counted] at hs
      scalar_tac
    simp only [WP.spec_ok, WP.uncurry']
    refine ⟨ hs, trivial, trivial, ?_ ⟩
    intro i hclaim
    exact none_missing_when_all_done s.chunks.val hnone i hclaim.1
  · rename_i hnonzero
    have hsome : 0 < s.chunks.val.length := by
      have hle : notDone s.chunks.val ≤ s.chunks.val.length := List.countP_le_length
      simp only [schedule.Schedule.Counted] at hs
      scalar_tac
    step as ⟨ index, hindex ⟩
    have hindexlt : index.val < s.chunks.val.length := by
      rw [hindex, htotal]
      exact Nat.mod_lt _ hsome
    have hinv : claimInv s.chunks.val available index.val (index, 0#u64) := by
      refine ⟨ by scalar_tac, ?_, ?_ ⟩
      · simp [ringAt, hindexlt]
      · intro k hk
        simp at hk
    step with schedule.Schedule.claim_loop.spec s.chunks available cursor total index 0#u64
      index.val hsized htotal hindexlt hinv as ⟨ found, chunks', cursor', hpost ⟩
    cases found with
    | some chosen =>
      obtain ⟨ hclaim, hchunks, hcursor ⟩ := hpost
      -- A missing chunk and one being fetched both count as not done.
      have hcount := notDone_set s.chunks.val chosen.val ChunkState.Missing
        ChunkState.InFlight hclaim.1
      refine ⟨ ?_, hclaim, hchunks, hcursor ⟩
      simp only [schedule.Schedule.Counted] at hs ⊢
      simp at hcount
      rw [hchunks, hcount]
      exact hs
    | none =>
      obtain ⟨ hchunks, hcursor, hnone ⟩ := hpost
      subst hchunks
      exact ⟨ hs, rfl, hcursor, hnone ⟩

/-- `is_complete` says yes exactly when every chunk is done. -/
@[step]
theorem schedule.Schedule.is_complete.spec (s : Schedule) (hs : s.Counted) :
    schedule.Schedule.is_complete s
    ⦃ r => r = true ↔ ∀ state ∈ s.chunks.val, state = ChunkState.Done ⦄ := by
  unfold schedule.Schedule.is_complete
  simp only [WP.spec_ok, schedule.Schedule.Counted] at *
  have hzero : s.remaining = 0#u64 ↔ notDone s.chunks.val = 0 := by
    rw [UScalar.eq_equiv, hs]
    rfl
  simp only [decide_eq_true_eq, hzero, notDone, List.countP_eq_zero]
  simp

/-- `state` returns the state of a chunk, and nothing for an index past the
end. -/
@[step]
theorem schedule.Schedule.state.spec (s : Schedule) (index : U64) :
    schedule.Schedule.state s index ⦃ r => r = s.chunks.val[index.val]? ⦄ := by
  unfold schedule.Schedule.state
  step as ⟨ slot, hfits, hover ⟩
  split
  · rename_i hin
    have hslot : slot.val = index.val := by
      by_cases h : index.val ≤ Usize.max
      · exact hfits h
      · have := hover (by omega)
        scalar_tac
    have hidx : index.val < s.chunks.val.length := by scalar_tac
    step as ⟨ state, hstate ⟩
    rw [List.getElem?_eq_getElem hidx]
    simp_all
  · rename_i hout
    have hpast : s.chunks.val.length ≤ index.val := by
      by_cases h : index.val ≤ Usize.max
      · have := hfits h
        scalar_tac
      · scalar_tac
    simp [List.getElem?_eq_none hpast]

end model
