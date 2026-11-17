-- What is proven about `model::mirror::step`.
--
-- A sync makes a destination hold what a source holds. For each path it
-- takes one step, which `decide` chooses from what the two have there.
--
-- A thing at a path is modelled here as its kind, what it holds, and its
-- permissions and time. `decide` is not shown what two things hold. It is
-- told whether they hold the same, and these theorems are about any two
-- things of which it was told the truth.
--
--   * With leave to remove, after the step the destination has at the path
--     exactly what the source has there: the same thing, or nothing if the
--     source has nothing.
--   * Without leave to remove, the same is true wherever the source has
--     something, unless the step is `Blocked`. What only the destination
--     has stays as it is.
--   * A step removes only where the source has nothing.
--   * A step is `Blocked` only without leave to remove, and only where the
--     destination has a directory and the source has something that is not
--     one.
--   * Where the destination already has what the source has, the step is
--     `Keep`. So a second sync does nothing.
--   * `decide` cannot fail.
module
public import Aeneas
public import Model.Funs
@[expose] public section
open Aeneas Aeneas.Std Result

namespace model

open mirror.step

deriving instance DecidableEq for mirror.step.Kind
deriving instance DecidableEq for mirror.step.Step

/-- A thing at a path: its kind, what it holds, and its permissions and
time. -/
structure Thing (C M : Type) where
  kind : Kind
  content : C
  manner : M

variable {C M : Type} [DecidableEq C] [DecidableEq M]

/-- What `decide` is told about a path where the source has `s` and the
destination has `d`. Nothing, if neither has anything there: such a path is
not one a sync meets. -/
def told : Option (Thing C M) → Option (Thing C M) → Option Pair
  | none, none => none
  | some _, none => some Pair.OnlySource
  | none, some _ => some Pair.OnlyDestination
  | some s, some d =>
    some (Pair.Both s.kind d.kind (decide (s.content = d.content)) (decide (s.manner = d.manner)))

/-- What the destination has at a path once `step` is taken there. -/
def after (step : Step) (s d : Option (Thing C M)) : Option (Thing C M) :=
  match step with
  | Step.Keep => d
  | Step.Blocked => d
  | Step.Add => s
  | Step.Replace => s
  | Step.Remove => none
  | Step.Touch =>
    match s, d with
    | some s, some d => some { d with manner := s.manner }
    | _, _ => d

/-- `decide` cannot fail. -/
theorem mirror.step.decide.total (pair : Pair) (remove : Bool) :
    ∃ step, mirror.step.decide pair remove = ok step := by
  unfold mirror.step.decide mirror.step.same_kind mirror.step.is_directory
  cases pair with
  | OnlySource => simp
  | OnlyDestination => cases remove <;> simp
  | Both source destination same_content same_manner =>
    cases source <;> cases destination <;> cases same_content <;> cases same_manner <;>
      cases remove <;> simp

/-- With leave to remove, the destination has after the step exactly what
the source has. -/
theorem with_leave_to_remove_the_destination_becomes_the_source
    (s d : Option (Thing C M)) (pair : Pair) (step : Step)
    (h : told s d = some pair) (hd : mirror.step.decide pair true = ok step) :
    after step s d = s := by
  unfold mirror.step.decide mirror.step.same_kind mirror.step.is_directory at hd
  cases s with
  | none =>
    cases d with
    | none => simp [told] at h
    | some d => simp [told] at h; subst h; simp at hd; subst hd; simp [after]
  | some s =>
    cases d with
    | none => simp [told] at h; subst h; simp at hd; subst hd; simp [after]
    | some d =>
      obtain ⟨sk, sc, sm⟩ := s
      obtain ⟨dk, dc, dm⟩ := d
      simp [told] at h; subst h
      cases sk <;> cases dk <;> by_cases hc : sc = dc <;> by_cases hm : sm = dm <;>
        simp [hc, hm] at hd <;> subst hd <;> simp_all [after]

/-- Without leave to remove, what the source has is at the destination
after the step, unless the step is `Blocked`. -/
theorem without_leave_what_the_source_has_arrives_unless_blocked
    (s : Thing C M) (d : Option (Thing C M)) (pair : Pair) (step : Step)
    (h : told (some s) d = some pair) (hd : mirror.step.decide pair false = ok step)
    (hb : step ≠ Step.Blocked) :
    after step (some s) d = some s := by
  unfold mirror.step.decide mirror.step.same_kind mirror.step.is_directory at hd
  cases d with
  | none => simp [told] at h; subst h; simp at hd; subst hd; simp [after]
  | some d =>
    obtain ⟨sk, sc, sm⟩ := s
    obtain ⟨dk, dc, dm⟩ := d
    simp [told] at h; subst h
    cases sk <;> cases dk <;> by_cases hc : sc = dc <;> by_cases hm : sm = dm <;>
      simp [hc, hm] at hd <;> subst hd <;> simp_all [after]

/-- Without leave to remove, what only the destination has stays as it
is. -/
theorem without_leave_what_only_the_destination_has_stays
    (d : Thing C M) (pair : Pair) (step : Step)
    (h : told (none : Option (Thing C M)) (some d) = some pair)
    (hd : mirror.step.decide pair false = ok step) :
    after step none (some d) = some d := by
  unfold mirror.step.decide at hd
  simp [told] at h; subst h
  simp at hd; subst hd
  simp [after]

/-- A step removes only where the source has nothing. -/
theorem removed_only_where_the_source_has_nothing
    (s d : Option (Thing C M)) (pair : Pair) (remove : Bool)
    (h : told s d = some pair) (hd : mirror.step.decide pair remove = ok Step.Remove) :
    s = none := by
  unfold mirror.step.decide mirror.step.same_kind mirror.step.is_directory at hd
  cases s with
  | none => rfl
  | some s =>
    cases d with
    | none => simp [told] at h; subst h; simp at hd
    | some d =>
      obtain ⟨sk, sc, sm⟩ := s
      obtain ⟨dk, dc, dm⟩ := d
      simp [told] at h; subst h
      cases sk <;> cases dk <;> by_cases hc : sc = dc <;> by_cases hm : sm = dm <;>
        cases remove <;> simp [hc, hm] at hd

/-- A step is `Blocked` only without leave to remove, and only where the
destination has a directory and the source has something that is not
one. -/
theorem blocked_only_by_a_directory_and_without_leave
    (s d : Option (Thing C M)) (pair : Pair) (remove : Bool)
    (h : told s d = some pair) (hd : mirror.step.decide pair remove = ok Step.Blocked) :
    remove = false ∧
      ∃ s' d', s = some s' ∧ d = some d' ∧
        d'.kind = Kind.Directory ∧ s'.kind ≠ Kind.Directory := by
  unfold mirror.step.decide mirror.step.same_kind mirror.step.is_directory at hd
  cases s with
  | none =>
    cases d with
    | none => simp [told] at h
    | some d => simp [told] at h; subst h; cases remove <;> simp at hd
  | some s =>
    cases d with
    | none => simp [told] at h; subst h; simp at hd
    | some d =>
      obtain ⟨sk, sc, sm⟩ := s
      obtain ⟨dk, dc, dm⟩ := d
      simp [told] at h; subst h
      cases sk <;> cases dk <;> by_cases hc : sc = dc <;> by_cases hm : sm = dm <;>
        cases remove <;> simp [hc, hm] at hd <;> simp

/-- Where the destination has what the source has, the step is `Keep`: a
second sync does nothing. -/
theorem a_second_sync_does_nothing
    (s : Thing C M) (pair : Pair) (remove : Bool)
    (h : told (some s) (some s) = some pair) :
    mirror.step.decide pair remove = ok Step.Keep := by
  unfold mirror.step.decide mirror.step.same_kind mirror.step.is_directory
  obtain ⟨sk, sc, sm⟩ := s
  simp [told] at h; subst h
  cases sk <;> simp

end model
