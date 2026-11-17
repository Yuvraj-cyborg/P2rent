//! Which chunk to fetch next.
//!
//! A [`Schedule`] is a pure state machine. It holds no lock, clock or task:
//! the caller owns the concurrency and tells the schedule what happened.

use crate::artifact::{Bitfield, to_usize};
use alloc::vec::Vec;

/// Names whoever is fetching a chunk. The caller chooses the numbers, one
/// for each supplier it runs.
pub type Owner = u32;

/// Where one chunk stands in a transfer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChunkState {
    /// Not held, and nobody is fetching it.
    Missing,
    /// Reserved by exactly one fetcher, which is named.
    InFlight(Owner),
    /// Verified and stored.
    Done,
}

/// How the fetch of a chunk ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The bytes arrived, matched what was expected and are stored.
    Verified,
    /// The fetch failed or was given up, or it brought the wrong bytes.
    Failed,
}

/// The state of every chunk in one transfer.
///
/// `remaining` always equals the number of chunks that are not
/// [`ChunkState::Done`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Schedule {
    chunks: Vec<ChunkState>,
    remaining: u64,
}

impl Schedule {
    /// Starts from what is already held: a chunk whose bit is set is done,
    /// and every other chunk is missing.
    pub fn new(have: &Bitfield) -> Self {
        let total = have.len();
        let mut chunks = Vec::with_capacity(to_usize(total));
        let mut remaining = 0;
        let mut index = 0;
        while index < total {
            if have.get(index) {
                chunks.push(ChunkState::Done);
            } else {
                chunks.push(ChunkState::Missing);
                remaining += 1;
            }
            index += 1;
        }
        Self { chunks, remaining }
    }

    /// Number of chunks in the transfer.
    pub fn len(&self) -> u64 {
        self.chunks.len() as u64
    }

    /// Whether the transfer has no chunks at all.
    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }

    /// Number of chunks that are not done yet.
    pub fn remaining(&self) -> u64 {
        self.remaining
    }

    /// Whether every chunk is done.
    pub fn is_complete(&self) -> bool {
        self.remaining == 0
    }

    /// The state of chunk `index`, or `None` if it is out of range.
    pub fn state(&self, index: u64) -> Option<ChunkState> {
        let slot = to_usize(index);
        if slot < self.chunks.len() {
            Some(self.chunks[slot])
        } else {
            None
        }
    }

    /// Reserves for `owner` a missing chunk that `available` holds and
    /// returns its index, or `None` if there is no such chunk.
    ///
    /// The search starts at `cursor` and wraps round. On success the cursor
    /// moves just past the chosen chunk, so the next search carries on from
    /// there. Fetchers that start from different cursors therefore collect
    /// different chunks first and can trade them afterwards.
    pub fn claim(&mut self, owner: Owner, available: &Bitfield, cursor: &mut u64) -> Option<u64> {
        let total = self.len();
        if self.remaining == 0 {
            return None;
        }
        // One division to bring the cursor into range. After that, wrapping
        // is a comparison, which is cheaper to run and simpler to reason
        // about than a remainder on every step.
        let mut index = *cursor % total;
        let mut step = 0;
        while step < total {
            let slot = to_usize(index);
            let next = if index + 1 == total { 0 } else { index + 1 };
            if is_missing(self.chunks[slot]) && available.get(index) {
                self.chunks[slot] = ChunkState::InFlight(owner);
                *cursor = next;
                return Some(index);
            }
            index = next;
            step += 1;
        }
        None
    }

    /// Ends the fetch of a chunk that `owner` reserved. A verified chunk
    /// becomes done, which is the only way a chunk becomes done. A failed
    /// one goes back to missing, to be claimed again.
    ///
    /// Returns `false`, changing nothing, unless the chunk is in flight and
    /// `owner` is the one fetching it.
    pub fn settle(&mut self, index: u64, owner: Owner, outcome: Outcome) -> bool {
        let slot = to_usize(index);
        if slot < self.chunks.len() && is_held_by(self.chunks[slot], owner) {
            match outcome {
                Outcome::Verified => {
                    self.chunks[slot] = ChunkState::Done;
                    self.remaining -= 1;
                }
                Outcome::Failed => self.chunks[slot] = ChunkState::Missing,
            }
            true
        } else {
            false
        }
    }
}

fn is_missing(state: ChunkState) -> bool {
    matches!(state, ChunkState::Missing)
}

/// Whether `owner` is the one fetching a chunk in this state.
fn is_held_by(state: ChunkState, owner: Owner) -> bool {
    match state {
        ChunkState::InFlight(by) => by == owner,
        ChunkState::Missing | ChunkState::Done => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: Owner = 7;
    const OTHER: Owner = 8;

    fn states(schedule: &Schedule) -> Vec<ChunkState> {
        (0..schedule.len())
            .filter_map(|i| schedule.state(i))
            .collect()
    }

    #[test]
    fn starts_from_what_is_held() {
        let mut have = Bitfield::new(3);
        have.set(1);
        let schedule = Schedule::new(&have);
        assert_eq!(
            states(&schedule),
            [ChunkState::Missing, ChunkState::Done, ChunkState::Missing]
        );
        assert_eq!(schedule.remaining(), 2);
        assert!(!schedule.is_complete());
        assert!(Schedule::new(&Bitfield::new(0)).is_complete());
        assert!(!schedule.is_empty());
        assert!(Schedule::new(&Bitfield::new(0)).is_empty());
    }

    #[test]
    fn claims_wrap_round_from_the_cursor() {
        let mut schedule = Schedule::new(&Bitfield::new(3));
        let everything = Bitfield::full(3);
        let mut cursor = 2;
        assert_eq!(schedule.claim(ME, &everything, &mut cursor), Some(2));
        assert_eq!(cursor, 0);
        assert_eq!(schedule.claim(ME, &everything, &mut cursor), Some(0));
        assert_eq!(schedule.claim(ME, &everything, &mut cursor), Some(1));
        assert_eq!(schedule.claim(ME, &everything, &mut cursor), None);
    }

    #[test]
    fn only_claims_what_the_supplier_holds() {
        let mut schedule = Schedule::new(&Bitfield::new(3));
        let mut only_last = Bitfield::new(3);
        only_last.set(2);
        let mut cursor = 0;
        assert_eq!(schedule.claim(ME, &only_last, &mut cursor), Some(2));
        assert_eq!(schedule.claim(ME, &only_last, &mut cursor), None);
    }

    #[test]
    fn a_cursor_beyond_the_end_is_wrapped() {
        let mut schedule = Schedule::new(&Bitfield::new(3));
        let mut cursor = u64::MAX;
        assert_eq!(
            schedule.claim(ME, &Bitfield::full(3), &mut cursor),
            Some(u64::MAX % 3)
        );
    }

    #[test]
    fn a_claimed_chunk_names_its_fetcher() {
        let mut schedule = Schedule::new(&Bitfield::new(2));
        let everything = Bitfield::full(2);
        let mut cursor = 0;
        assert_eq!(schedule.claim(ME, &everything, &mut cursor), Some(0));
        assert_eq!(schedule.claim(OTHER, &everything, &mut cursor), Some(1));
        assert_eq!(
            states(&schedule),
            [ChunkState::InFlight(ME), ChunkState::InFlight(OTHER)]
        );
    }

    #[test]
    fn a_failed_chunk_can_be_claimed_again() {
        let mut schedule = Schedule::new(&Bitfield::new(1));
        let everything = Bitfield::full(1);
        let mut cursor = 0;
        assert_eq!(schedule.claim(ME, &everything, &mut cursor), Some(0));
        assert!(schedule.settle(0, ME, Outcome::Failed));
        assert_eq!(schedule.state(0), Some(ChunkState::Missing));
        assert_eq!(schedule.remaining(), 1);
        assert_eq!(schedule.claim(OTHER, &everything, &mut cursor), Some(0));
        assert!(schedule.settle(0, OTHER, Outcome::Verified));
        assert_eq!(schedule.state(0), Some(ChunkState::Done));
        assert!(schedule.is_complete());
    }

    #[test]
    fn only_the_fetcher_of_a_chunk_can_settle_it() {
        let mut schedule = Schedule::new(&Bitfield::new(1));
        let mut cursor = 0;
        assert_eq!(schedule.claim(ME, &Bitfield::full(1), &mut cursor), Some(0));
        let before = schedule.clone();
        assert!(!schedule.settle(0, OTHER, Outcome::Verified));
        assert!(!schedule.settle(0, OTHER, Outcome::Failed));
        assert_eq!(schedule, before);
        assert!(schedule.settle(0, ME, Outcome::Verified));
        // Done is final: nobody settles it again.
        assert!(!schedule.settle(0, ME, Outcome::Failed));
        assert!(schedule.is_complete());
    }

    #[test]
    fn settling_an_unclaimed_chunk_changes_nothing() {
        let mut schedule = Schedule::new(&Bitfield::new(2));
        let before = schedule.clone();
        assert!(!schedule.settle(0, ME, Outcome::Verified));
        assert!(!schedule.settle(1, ME, Outcome::Failed));
        assert!(!schedule.settle(9, ME, Outcome::Verified));
        assert!(!schedule.settle(9, ME, Outcome::Failed));
        assert_eq!(schedule, before);
    }
}

/// Bounded proofs, run by `just kani`. They hold for every schedule of up to
/// `MAX_CHUNKS` chunks, in every state it can reach, and for every argument.
///
/// Each proof walks through the lengths one at a time. The model checker is
/// far quicker when the length is a known number and only the contents are
/// left open.
#[cfg(kani)]
mod proofs {
    use super::*;

    const MAX_CHUNKS: u64 = 4;

    /// Any set of chunk indices below `len`.
    fn any_bitfield(len: u64) -> Bitfield {
        let mut bits = Bitfield::new(len);
        let mut index = 0;
        while index < len {
            if kani::any() {
                bits.set(index);
            }
            index += 1;
        }
        bits
    }

    /// A schedule of `len` chunks in any state it can reach: each chunk
    /// missing, in flight with any fetcher, or done.
    fn any_schedule(len: u64) -> Schedule {
        let mut schedule = Schedule::new(&any_bitfield(len));
        let mut index = 0;
        while index < len {
            if schedule.chunks[to_usize(index)] == ChunkState::Missing && kani::any() {
                schedule.chunks[to_usize(index)] = ChunkState::InFlight(kani::any());
            }
            index += 1;
        }
        schedule
    }

    fn not_done(schedule: &Schedule) -> u64 {
        let mut count = 0;
        let mut index = 0;
        while index < schedule.len() {
            if schedule.chunks[to_usize(index)] != ChunkState::Done {
                count += 1;
            }
            index += 1;
        }
        count
    }

    #[kani::proof]
    #[kani::unwind(6)]
    fn a_new_schedule_counts_what_is_missing() {
        let mut len = 0;
        while len <= MAX_CHUNKS {
            let have = any_bitfield(len);
            let schedule = Schedule::new(&have);
            let index: u64 = kani::any();

            assert!(schedule.len() == len);
            assert!(schedule.remaining() == not_done(&schedule));
            match schedule.state(index) {
                None => assert!(index >= len),
                Some(ChunkState::Done) => assert!(have.get(index)),
                Some(ChunkState::Missing) => assert!(index < len && !have.get(index)),
                Some(ChunkState::InFlight(_)) => unreachable!(),
            }
            len += 1;
        }
    }

    /// A claim hands out only a missing chunk the supplier holds, marks
    /// exactly that chunk as in flight with the claimant as its fetcher, and
    /// refuses only when there is no such chunk.
    #[kani::proof]
    #[kani::unwind(6)]
    fn a_claim_reserves_one_missing_chunk_the_supplier_holds() {
        let mut len = 0;
        while len <= MAX_CHUNKS {
            let mut schedule = any_schedule(len);
            let before = schedule.clone();
            let available = any_bitfield(len);
            let owner: Owner = kani::any();
            let mut cursor: u64 = kani::any();
            let other: u64 = kani::any();

            match schedule.claim(owner, &available, &mut cursor) {
                Some(index) => {
                    assert!(before.state(index) == Some(ChunkState::Missing));
                    assert!(available.get(index));
                    assert!(schedule.state(index) == Some(ChunkState::InFlight(owner)));
                    assert!(cursor < len);
                    if other != index {
                        assert!(schedule.state(other) == before.state(other));
                    }
                }
                None => {
                    assert!(schedule.state(other) == before.state(other));
                    if before.state(other) == Some(ChunkState::Missing) {
                        assert!(!available.get(other));
                    }
                }
            }
            assert!(schedule.remaining() == not_done(&schedule));
            len += 1;
        }
    }

    /// Settling acts only on a chunk in flight, only for its fetcher, and
    /// only on that chunk. A chunk becomes done only when its fetcher
    /// reports it verified. The count of chunks left stays right throughout.
    #[kani::proof]
    #[kani::unwind(6)]
    fn only_its_fetcher_settles_a_chunk_and_only_verified_is_done() {
        let mut len = 0;
        while len <= MAX_CHUNKS {
            let mut schedule = any_schedule(len);
            let before = schedule.clone();
            let index: u64 = kani::any();
            let other: u64 = kani::any();
            kani::assume(other != index);
            let owner: Owner = kani::any();
            let verified: bool = kani::any();
            let outcome = if verified {
                Outcome::Verified
            } else {
                Outcome::Failed
            };
            let mine = before.state(index) == Some(ChunkState::InFlight(owner));

            assert!(schedule.settle(index, owner, outcome) == mine);
            if mine && verified {
                assert!(schedule.state(index) == Some(ChunkState::Done));
                assert!(schedule.remaining() + 1 == before.remaining());
            } else if mine {
                assert!(schedule.state(index) == Some(ChunkState::Missing));
                assert!(schedule.remaining() == before.remaining());
            } else {
                assert!(schedule.state(index) == before.state(index));
                assert!(schedule.remaining() == before.remaining());
            }
            // Nothing becomes done any other way.
            if schedule.state(index) == Some(ChunkState::Done) {
                assert!(before.state(index) == Some(ChunkState::Done) || (mine && verified));
            }
            assert!(schedule.state(other) == before.state(other));
            assert!(schedule.remaining() == not_done(&schedule));
            len += 1;
        }
    }
}
