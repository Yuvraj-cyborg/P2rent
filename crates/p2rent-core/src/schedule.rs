//! Which chunk to fetch next.
//!
//! A [`Schedule`] is a pure state machine. It holds no lock, clock or task:
//! the caller owns the concurrency and tells the schedule what happened.

use crate::artifact::{Bitfield, to_usize};
use alloc::vec::Vec;

/// Where one chunk stands in a transfer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChunkState {
    /// Not held, and nobody is fetching it.
    Missing,
    /// Reserved by exactly one fetcher.
    InFlight,
    /// Verified and stored.
    Done,
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
        self.chunks.get(to_usize(index)).copied()
    }

    /// Reserves a missing chunk that `available` holds and returns its
    /// index, or `None` if there is no such chunk.
    ///
    /// The search starts at `cursor` and wraps round. On success the cursor
    /// moves just past the chosen chunk, so the next search carries on from
    /// there. Fetchers that start from different cursors therefore collect
    /// different chunks first and can trade them afterwards.
    pub fn claim(&mut self, available: &Bitfield, cursor: &mut u64) -> Option<u64> {
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
            if self.chunks[slot] == ChunkState::Missing && available.get(index) {
                self.chunks[slot] = ChunkState::InFlight;
                *cursor = next;
                return Some(index);
            }
            index = next;
            step += 1;
        }
        None
    }

    /// Records that a claimed chunk was verified and stored.
    ///
    /// Returns `false`, changing nothing, if the chunk was not in flight.
    pub fn complete(&mut self, index: u64) -> bool {
        let settled = self.settle(index, ChunkState::Done);
        if settled {
            self.remaining -= 1;
        }
        settled
    }

    /// Puts a claimed chunk back, so that it can be claimed again.
    ///
    /// Returns `false`, changing nothing, if the chunk was not in flight.
    pub fn release(&mut self, index: u64) -> bool {
        self.settle(index, ChunkState::Missing)
    }

    /// Moves a chunk that is in flight to `state`. Does nothing and returns
    /// `false` for any other chunk, or for an index out of range.
    fn settle(&mut self, index: u64, state: ChunkState) -> bool {
        let slot = to_usize(index);
        if slot < self.chunks.len() && self.chunks[slot] == ChunkState::InFlight {
            self.chunks[slot] = state;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }

    #[test]
    fn claims_wrap_round_from_the_cursor() {
        let mut schedule = Schedule::new(&Bitfield::new(3));
        let everything = Bitfield::full(3);
        let mut cursor = 2;
        assert_eq!(schedule.claim(&everything, &mut cursor), Some(2));
        assert_eq!(cursor, 0);
        assert_eq!(schedule.claim(&everything, &mut cursor), Some(0));
        assert_eq!(schedule.claim(&everything, &mut cursor), Some(1));
        assert_eq!(schedule.claim(&everything, &mut cursor), None);
    }

    #[test]
    fn only_claims_what_the_supplier_holds() {
        let mut schedule = Schedule::new(&Bitfield::new(3));
        let mut only_last = Bitfield::new(3);
        only_last.set(2);
        let mut cursor = 0;
        assert_eq!(schedule.claim(&only_last, &mut cursor), Some(2));
        assert_eq!(schedule.claim(&only_last, &mut cursor), None);
    }

    #[test]
    fn a_cursor_beyond_the_end_is_wrapped() {
        let mut schedule = Schedule::new(&Bitfield::new(3));
        let mut cursor = u64::MAX;
        assert_eq!(
            schedule.claim(&Bitfield::full(3), &mut cursor),
            Some(u64::MAX % 3)
        );
    }

    #[test]
    fn a_released_chunk_can_be_claimed_again() {
        let mut schedule = Schedule::new(&Bitfield::new(1));
        let everything = Bitfield::full(1);
        let mut cursor = 0;
        assert_eq!(schedule.claim(&everything, &mut cursor), Some(0));
        assert!(schedule.release(0));
        assert_eq!(schedule.state(0), Some(ChunkState::Missing));
        assert_eq!(schedule.claim(&everything, &mut cursor), Some(0));
        assert!(schedule.complete(0));
        assert!(schedule.is_complete());
    }

    #[test]
    fn completing_or_releasing_an_unclaimed_chunk_changes_nothing() {
        let mut schedule = Schedule::new(&Bitfield::new(2));
        let before = schedule.clone();
        assert!(!schedule.complete(0));
        assert!(!schedule.release(1));
        assert!(!schedule.complete(9));
        assert!(!schedule.release(9));
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
    /// missing, in flight or done.
    fn any_schedule(len: u64) -> Schedule {
        let mut schedule = Schedule::new(&any_bitfield(len));
        let mut index = 0;
        while index < len {
            if schedule.chunks[to_usize(index)] == ChunkState::Missing && kani::any() {
                schedule.chunks[to_usize(index)] = ChunkState::InFlight;
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
                Some(ChunkState::InFlight) => unreachable!(),
            }
            len += 1;
        }
    }

    /// A claim hands out only a missing chunk the supplier holds, marks
    /// exactly that chunk as in flight, and refuses only when there is no
    /// such chunk.
    #[kani::proof]
    #[kani::unwind(6)]
    fn a_claim_reserves_one_missing_chunk_the_supplier_holds() {
        let mut len = 0;
        while len <= MAX_CHUNKS {
            let mut schedule = any_schedule(len);
            let before = schedule.clone();
            let available = any_bitfield(len);
            let mut cursor: u64 = kani::any();
            let other: u64 = kani::any();

            match schedule.claim(&available, &mut cursor) {
                Some(index) => {
                    assert!(before.state(index) == Some(ChunkState::Missing));
                    assert!(available.get(index));
                    assert!(schedule.state(index) == Some(ChunkState::InFlight));
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

    /// Completing and releasing act only on a chunk that is in flight, and
    /// only on that chunk. The count of chunks left stays right throughout.
    #[kani::proof]
    #[kani::unwind(6)]
    fn completing_and_releasing_touch_only_a_chunk_in_flight() {
        let mut len = 0;
        while len <= MAX_CHUNKS {
            let mut schedule = any_schedule(len);
            let before = schedule.clone();
            let index: u64 = kani::any();
            let other: u64 = kani::any();
            kani::assume(other != index);
            let in_flight = before.state(index) == Some(ChunkState::InFlight);

            if kani::any() {
                assert!(schedule.complete(index) == in_flight);
                if in_flight {
                    assert!(schedule.state(index) == Some(ChunkState::Done));
                    assert!(schedule.remaining() + 1 == before.remaining());
                }
            } else {
                assert!(schedule.release(index) == in_flight);
                if in_flight {
                    assert!(schedule.state(index) == Some(ChunkState::Missing));
                    assert!(schedule.remaining() == before.remaining());
                }
            }
            if !in_flight {
                assert!(schedule.state(index) == before.state(index));
                assert!(schedule.remaining() == before.remaining());
            }
            assert!(schedule.state(other) == before.state(other));
            assert!(schedule.remaining() == not_done(&schedule));
            len += 1;
        }
    }
}
