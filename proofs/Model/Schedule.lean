-- What is proven about `model::schedule`.
--
-- A schedule records, for each chunk of a transfer, whether it is missing,
-- being fetched or done, and how many are not done yet. A chunk being
-- fetched names the one fetcher that reserved it.
--
--   * A chunk is in exactly one of the three states, and a chunk being
--     fetched has exactly one fetcher: that is what the type says.
--   * `settle` changes a chunk only if it is being fetched and only for
--     its fetcher. For anyone else, and for any other chunk, it changes
--     nothing.
--   * A chunk becomes done in one way only: its fetcher settles it as
--     verified. `claim` and a failed fetch never make a chunk done.
--   * The count of chunks not yet done stays right through every operation.
--   * `claim` hands out only a chunk that is missing and that the supplier
--     holds, marks exactly that chunk as being fetched by the claimant, and
--     otherwise changes nothing.
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

/-- `is_missing` says whether a chunk is missing. -/
@[step]
theorem schedule.is_missing.spec (state : ChunkState) :
    schedule.is_missing state ⦃ r => r = decide (state = ChunkState.Missing) ⦄ := by
  unfold schedule.is_missing
  cases state <;> simp

/-- `is_held_by` says whether a chunk is being fetched by the fetcher named. -/
@[step]
theorem schedule.is_held_by.spec (state : ChunkState) (owner : U32) :
    schedule.is_held_by state owner
    ⦃ r => r = decide (state = ChunkState.InFlight owner) ⦄ := by
  unfold schedule.is_held_by
  cases state <;> simp

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

/-- What `settle` does. If the chunk is being fetched by the fetcher named,
it becomes done when the outcome is "verified" and missing when the fetch
failed, and no other chunk changes. Otherwise nothing changes at all. -/
def schedule.Schedule.Settled (s : Schedule) (index : U64) (owner : U32)
    (outcome : Outcome) (settled : Bool) (s' : Schedule) : Prop :=
  if s.chunks.val[index.val]? = some (ChunkState.InFlight owner) then
    settled = true ∧
    match outcome with
    | Outcome.Verified =>
      s'.chunks.val = s.chunks.val.set index.val ChunkState.Done ∧
      s'.remaining.val + 1 = s.remaining.val
    | Outcome.Failed =>
      s'.chunks.val = s.chunks.val.set index.val ChunkState.Missing ∧
      s'.remaining = s.remaining
  else settled = false ∧ s' = s

/-- `settle` does what `Settled` says, keeps the count right, and cannot
fail. -/
@[step]
theorem schedule.Schedule.settle.spec (s : Schedule) (index : U64) (owner : U32)
    (outcome : Outcome) (hs : s.Counted) :
    schedule.Schedule.settle s index owner outcome
    ⦃ settled s' => s'.Counted ∧ s.Settled index owner outcome settled s' ⦄ := by
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
    step as ⟨ cs, hcs ⟩
    step as ⟨ b, hb ⟩
    have hstate : s.chunks.val[index.val]? = some cs := by
      rw [List.getElem?_eq_getElem hidx]
      simp_all
    split
    · rename_i hmine
      have hflight : s.chunks.val[index.val]? = some (ChunkState.InFlight owner) := by
        simp_all
      cases outcome with
      | Verified =>
        have hcount := notDone_set s.chunks.val index.val (ChunkState.InFlight owner)
          ChunkState.Done hflight
        simp at hcount
        simp only [Schedule.Counted] at hs
        step as ⟨ old, back, hold, hback ⟩
        step as ⟨ remaining, hsub ⟩
        simp only [Schedule.Counted, Schedule.Settled, hflight, ↓reduceIte]
        simp_all
        omega
      | Failed =>
        have hcount := notDone_set s.chunks.val index.val (ChunkState.InFlight owner)
          ChunkState.Missing hflight
        simp at hcount
        simp only [Schedule.Counted] at hs
        step as ⟨ old, back, hold, hback ⟩
        simp only [Schedule.Counted, Schedule.Settled, hflight, ↓reduceIte]
        simp_all
    · rename_i hnotmine
      have hother : s.chunks.val[index.val]? ≠ some (ChunkState.InFlight owner) := by
        simp_all
      simp [Schedule.Settled, hother, hs]
  · rename_i hout
    have hlen := hpast (by scalar_tac)
    have hnone : s.chunks.val[index.val]? = none := List.getElem?_eq_none hlen
    simp [Schedule.Settled, hnone, hs]

/-- Only the fetcher of a chunk settles it. If someone else is fetching the
chunk, `settle` changes nothing and says so. -/
theorem schedule.Schedule.Settled.only_by_its_fetcher {s s' : Schedule} {index : U64}
    {owner other : U32} {outcome : Outcome} {settled : Bool}
    (h : s.Settled index other outcome settled s')
    (hflight : s.chunks.val[index.val]? = some (ChunkState.InFlight owner))
    (hother : other ≠ owner) : settled = false ∧ s' = s := by
  unfold Schedule.Settled at h
  have hnot : ¬ s.chunks.val[index.val]? = some (ChunkState.InFlight other) := by
    rw [hflight]
    intro heq
    cases heq
    exact hother rfl
  simpa [hnot] using h

/-- A chunk becomes done only when its fetcher settles it as verified. If a
chunk is done after `settle`, either it was done before, or it is the chunk
settled, the one settling it was its fetcher, and the outcome was
"verified". -/
theorem schedule.Schedule.Settled.done_only_when_verified {s s' : Schedule}
    {index : U64} {owner : U32} {outcome : Outcome} {settled : Bool}
    (h : s.Settled index owner outcome settled s') (j : Nat)
    (hdone : s'.chunks.val[j]? = some ChunkState.Done) :
    s.chunks.val[j]? = some ChunkState.Done ∨
      (j = index.val ∧ outcome = Outcome.Verified ∧
        s.chunks.val[j]? = some (ChunkState.InFlight owner)) := by
  unfold Schedule.Settled at h
  split at h
  · rename_i hflight
    obtain ⟨ _, h ⟩ := h
    by_cases hj : j = index.val
    · subst hj
      cases outcome with
      | Verified => exact Or.inr ⟨ rfl, rfl, hflight ⟩
      | Failed =>
        obtain ⟨ hchunks, _ ⟩ := h
        rw [hchunks] at hdone
        simp [List.getElem?_set] at hdone
    · have hne : index.val ≠ j := fun heq => hj heq.symm
      cases outcome with
      | Verified =>
        obtain ⟨ hchunks, _ ⟩ := h
        rw [hchunks, List.getElem?_set_ne hne] at hdone
        exact Or.inl hdone
      | Failed =>
        obtain ⟨ hchunks, _ ⟩ := h
        rw [hchunks, List.getElem?_set_ne hne] at hdone
        exact Or.inl hdone
  · obtain ⟨ _, hsame ⟩ := h
    subst hsame
    exact Or.inl hdone

/-- Marking a chunk as being fetched makes no chunk done. This is what
`claim` does to the list, so `claim` never makes a chunk done. -/
theorem claiming_makes_nothing_done (chunks : List ChunkState) (i j : Nat) (owner : U32)
    (hdone : (chunks.set i (ChunkState.InFlight owner))[j]? = some ChunkState.Done) :
    chunks[j]? = some ChunkState.Done := by
  by_cases hij : i = j
  · subst hij
    simp [List.getElem?_set] at hdone
  · rwa [List.getElem?_set_ne hij] at hdone

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
/-- `chosen` is the first chunk that can be claimed, going round the ring
from `start`: it is `passed` places on, and none of the places passed on
the way could be claimed. -/
def firstFrom (chunks : List ChunkState) (available : Bitfield) (start chosen : Nat) : Prop :=
  ∃ passed, passed < chunks.length ∧ chosen = ringAt chunks.length start passed ∧
    ∀ k, k < passed → ¬ claimable chunks available (ringAt chunks.length start k)

/-- The place after `i` on a ring of `total` places. -/
def ringNext (total i : Nat) : Nat :=
  if i + 1 = total then 0 else i + 1

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
theorem schedule.Schedule.claim_loop.spec (v : alloc.vec.Vec ChunkState) (owner : U32)
    (available : Bitfield) (cursor total index step : U64) (start : Nat)
    (hsized : available.Sized) (htotal : total.val = v.val.length)
    (hstart : start < v.val.length)
    (hinv : claimInv v.val available start (index, step)) :
    schedule.Schedule.claim_loop v owner available cursor total index step
    ⦃ found v' cursor' =>
      match found with
      | some chosen =>
        claimable v.val available chosen.val ∧
        v'.val = v.val.set chosen.val (ChunkState.InFlight owner) ∧
        cursor'.val < v.val.length ∧
        firstFrom v.val available start chosen.val ∧
        cursor'.val = ringNext v.val.length chosen.val
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
          refine ⟨ ⟨ hget, by rw [← hoffered]; exact hisoffered ⟩, ?_, hnextlt,
            ⟨ looked.val, hlooked, hat, hnone ⟩, by rw [hnextval, hring]; rfl ⟩
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
/-- `claim` hands out the first chunk, going round from the cursor, that is
missing and that the supplier holds. It marks exactly that chunk as being
fetched by the claimant and moves the cursor to the place after it. It
returns nothing only when no chunk at all could be claimed, and then
changes nothing. The count stays right either way, and the call cannot
fail. -/
@[step]
theorem schedule.Schedule.claim.spec (s : Schedule) (owner : U32) (available : Bitfield)
    (cursor : U64) (hs : s.Counted) (hsized : available.Sized) :
    schedule.Schedule.claim s owner available cursor
    ⦃ found s' cursor' =>
      s'.Counted ∧
      match found with
      | some chosen =>
        claimable s.chunks.val available chosen.val ∧
        s'.chunks.val = s.chunks.val.set chosen.val (ChunkState.InFlight owner) ∧
        s'.remaining = s.remaining ∧ cursor'.val < s.chunks.val.length ∧
        firstFrom s.chunks.val available (cursor.val % s.chunks.val.length) chosen.val ∧
        cursor'.val = ringNext s.chunks.val.length chosen.val
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
    step with schedule.Schedule.claim_loop.spec s.chunks owner available cursor total index 0#u64
      index.val hsized htotal hindexlt hinv as ⟨ found, chunks', cursor', hpost ⟩
    cases found with
    | some chosen =>
      obtain ⟨ hclaim, hchunks, hcursor, hfirst, hafter ⟩ := hpost
      -- A missing chunk and one being fetched both count as not done.
      have hcount := notDone_set s.chunks.val chosen.val ChunkState.Missing
        (ChunkState.InFlight owner) hclaim.1
      have hfrom : index.val = cursor.val % s.chunks.val.length := by
        rw [hindex, htotal]
      rw [hfrom] at hfirst
      refine ⟨ ?_, hclaim, hchunks, hcursor, hfirst, hafter ⟩
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

/-- `is_empty` says whether the transfer has no chunks at all. -/
@[step]
theorem schedule.Schedule.is_empty.spec (s : Schedule) :
    schedule.Schedule.is_empty s ⦃ r => r = decide (s.chunks.val.length = 0) ⦄ := by
  unfold schedule.Schedule.is_empty
  step*

/-- `remaining` gives the number of chunks that are not done yet. -/
@[step]
theorem schedule.Schedule.impl.remaining.spec (s : Schedule) (hs : s.Counted) :
    schedule.Schedule.impl.remaining s ⦃ r => r.val = notDone s.chunks.val ⦄ := by
  unfold schedule.Schedule.impl.remaining
  simp only [WP.spec_ok]
  exact hs

end model
