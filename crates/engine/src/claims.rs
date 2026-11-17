//! The schedule behind a lock, handing out claims that put themselves back.
//!
//! This is the only part of a download that several tasks change at once, so
//! it is kept free of everything else and is checked under every
//! interleaving of two fetchers by the model test at the bottom.

use model::artifact::Bitfield;
use model::schedule::{ChunkState, Outcome, Owner, Schedule};
use std::sync::PoisonError;

// The model test swaps in lock and reference-count types that let it
// explore every interleaving.
#[cfg(mux_loom)]
use loom::sync::{Arc, Mutex, MutexGuard};
#[cfg(not(mux_loom))]
use std::sync::{Arc, Mutex, MutexGuard};

/// How the rest of a transfer holds the claims. Its type follows the
/// reference count in use, which differs under the model test.
pub(crate) type Shared = Arc<Claims>;

/// Which chunks are missing, being fetched and done, shared by every task
/// working on one transfer.
pub(crate) struct Claims {
    schedule: Mutex<Schedule>,
    /// Called when a chunk is put back or the last one completes, so that
    /// fetchers with nothing to do look again.
    wake: Box<dyn Fn() + Send + Sync>,
}

impl Claims {
    pub(crate) fn new(have: &Bitfield, wake: impl Fn() + Send + Sync + 'static) -> Shared {
        Arc::new(Self {
            schedule: Mutex::new(Schedule::new(have)),
            wake: Box::new(wake),
        })
    }

    // A poisoned lock is recovered: see `crate::sync`.
    fn schedule(&self) -> MutexGuard<'_, Schedule> {
        self.schedule.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn is_complete(&self) -> bool {
        self.schedule().is_complete()
    }

    /// Whether the supplier fetching as `owner`, which holds `available`,
    /// has work: a chunk it is fetching, or one it holds that is missing
    /// and that nobody is fetching.
    pub(crate) fn has_work_for(&self, owner: Owner, available: &Bitfield) -> bool {
        let schedule = self.schedule();
        (0..schedule.len()).any(|index| match schedule.state(index) {
            Some(ChunkState::Missing) => available.get(index),
            Some(ChunkState::InFlight(by)) => by == owner,
            Some(ChunkState::Done) | None => false,
        })
    }

    /// Whether any chunk is reserved: on its way, or arrived and being
    /// stored.
    pub(crate) fn any_in_flight(&self) -> bool {
        let schedule = self.schedule();
        (0..schedule.len()).any(|index| is_in_flight(&schedule, Some(index)))
    }

    /// Reserves for `owner` a missing chunk that `available` holds,
    /// searching from `cursor`. See [`Schedule::claim`].
    ///
    /// A fetcher reads a run of chunks from one stream, and the chunk at
    /// its cursor carries its run on. When it cannot have that one, it
    /// starts another run at the first chunk it can have. But right behind
    /// a chunk that someone is fetching is where that fetcher's stream
    /// will be next: taken from there, the two would take turns and every
    /// chunk would need a stream of its own. So the new run starts further
    /// on, in the middle of what can be had from there.
    pub(crate) fn claim(
        claims: &Shared,
        owner: Owner,
        available: &Bitfield,
        cursor: &mut u64,
    ) -> Option<Claim> {
        let mut schedule = claims.schedule();
        let wanted = *cursor;
        let mut index = schedule.claim(owner, available, cursor)?;
        let carries_on = index == wanted % schedule.len();
        if !carries_on && is_in_flight(&schedule, index.checked_sub(1)) {
            let behind = (index + 1..schedule.len())
                .take_while(|&next| {
                    schedule.state(next) == Some(ChunkState::Missing) && available.get(next)
                })
                .count() as u64;
            schedule.settle(index, owner, Outcome::Failed);
            *cursor = index + behind.div_ceil(2);
            index = schedule.claim(owner, available, cursor)?;
        }
        drop(schedule);
        Some(Claim {
            claims: claims.clone(),
            index,
            owner,
            settled: false,
        })
    }

    /// Records a chunk that has been verified and stored.
    pub(crate) fn complete(&self, mut claim: Claim) {
        claim.settled = true;
        let finished = {
            let mut schedule = self.schedule();
            schedule.settle(claim.index, claim.owner, Outcome::Verified);
            schedule.is_complete()
        };
        if finished {
            (self.wake)();
        }
    }

    fn release(&self, index: u64, owner: Owner) {
        self.schedule().settle(index, owner, Outcome::Failed);
        (self.wake)();
    }
}

/// Whether someone is fetching the chunk at `index`, if there is one.
fn is_in_flight(schedule: &Schedule, index: Option<u64>) -> bool {
    let state = index.and_then(|index| schedule.state(index));
    matches!(state, Some(ChunkState::InFlight(_)))
}

/// A reserved chunk. Dropping it without completing puts the chunk back, so
/// a cancelled or failed request never strands a chunk.
pub(crate) struct Claim {
    claims: Shared,
    pub index: u64,
    owner: Owner,
    settled: bool,
}

impl Drop for Claim {
    fn drop(&mut self) {
        if !self.settled {
            self.claims.release(self.index, self.owner);
        }
    }
}

#[cfg(all(test, not(mux_loom)))]
mod tests {
    use super::*;

    #[test]
    fn a_chunk_is_in_flight_from_its_claim_until_it_is_settled() {
        let claims = Claims::new(&Bitfield::new(2), || {});
        let held = Bitfield::full(2);
        assert!(!claims.any_in_flight());

        let kept = Claims::claim(&claims, 1, &held, &mut 0).unwrap();
        let let_go = Claims::claim(&claims, 1, &held, &mut 0).unwrap();
        assert!(claims.any_in_flight());
        drop(let_go);
        assert!(claims.any_in_flight());
        claims.complete(kept);
        assert!(!claims.any_in_flight());
        assert!(!claims.is_complete());
    }

    /// Two fetchers, each reading a run of chunks from a stream of its
    /// own. One step lets a fetcher finish the chunk it has and take its
    /// next, and `quick` takes that many steps for each one of `slow`.
    /// Returns how many streams were opened: one for each run.
    fn streams_opened(chunks: u64, quick: usize) -> usize {
        let claims = Claims::new(&Bitfield::new(chunks), || {});
        let held = Bitfield::full(chunks);
        let mut cursors = [0, chunks / 2];
        let mut fetching: [Option<Claim>; 2] = [None, None];
        let mut streams = 0;
        while !claims.is_complete() {
            for (fetcher, steps) in [(0, quick), (1, 1)] {
                for _ in 0..steps {
                    if let Some(done) = fetching[fetcher].take() {
                        claims.complete(done);
                    }
                    let carried_on_from = cursors[fetcher];
                    let owner = Owner::try_from(fetcher).unwrap();
                    let next = Claims::claim(&claims, owner, &held, &mut cursors[fetcher]);
                    if next
                        .as_ref()
                        .is_some_and(|next| next.index != carried_on_from)
                    {
                        streams += 1;
                    }
                    fetching[fetcher] = next;
                }
            }
        }
        // The first chunk of each fetcher was the start of a stream too.
        streams + 2
    }

    /// When the quicker fetcher has finished its half, it goes on in the
    /// middle of what is left, again and again, and each time that is one
    /// stream more. Taking the chunk right behind the slower one's would
    /// be a stream for every chunk from there on: 87 of them at two to one.
    #[test]
    fn fetchers_that_meet_do_not_take_turns() {
        let streams = [1, 2, 3, 7].map(|quick| streams_opened(512, quick));
        assert_eq!(streams, [2, 7, 7, 8]);
    }

    #[test]
    fn a_new_run_starts_in_the_middle_of_what_another_is_reading_into() {
        let claims = Claims::new(&Bitfield::new(16), || {});
        let held = Bitfield::full(16);
        // One fetcher has read the first half, and another is at chunk 8.
        let mut first = 0;
        for _ in 0..8 {
            let claim = Claims::claim(&claims, 1, &held, &mut first).unwrap();
            claims.complete(claim);
        }
        let mut second = 8;
        let ahead = Claims::claim(&claims, 2, &held, &mut second).unwrap();
        assert_eq!(ahead.index, 8);

        // The first one's run has ended. Chunks 9 to 15 are left, and it
        // goes on from the middle of them, not from chunk 9.
        let jumped = Claims::claim(&claims, 1, &held, &mut first).unwrap();
        assert_eq!(jumped.index, 12);
        assert_eq!(first, 13);
        // So the other one's stream carries on.
        claims.complete(ahead);
        let carried_on = Claims::claim(&claims, 2, &held, &mut second).unwrap();
        assert_eq!(carried_on.index, 9);

        // With one chunk left behind another's, there is no middle.
        let claims = Claims::new(&Bitfield::new(3), || {});
        let held = Bitfield::full(3);
        let _ahead = Claims::claim(&claims, 2, &held, &mut 1).unwrap();
        let last = Claims::claim(&claims, 1, &held, &mut 1).unwrap();
        assert_eq!(last.index, 2);
    }

    /// The rule is for a new run that would start behind another's stream,
    /// and for nothing else.
    #[test]
    fn a_run_is_moved_only_when_it_is_new_and_right_behind_another() {
        // A new run, with nobody reading into it: it starts where it can.
        let claims = Claims::new(&Bitfield::new(8), || {});
        let held = Bitfield::full(8);
        let done = Claims::claim(&claims, 1, &held, &mut 4).unwrap();
        claims.complete(done);
        let after = Claims::claim(&claims, 1, &held, &mut 4).unwrap();
        assert_eq!(after.index, 5);

        // A run that carries on, though the chunk before it is now being
        // fetched by another: its own fetch of that chunk failed, and the
        // other took the chunk up.
        let claims = Claims::new(&Bitfield::new(8), || {});
        let mut cursor = 5;
        let failed = Claims::claim(&claims, 1, &held, &mut cursor).unwrap();
        assert_eq!((failed.index, cursor), (5, 6));
        drop(failed);
        let _taken_up = Claims::claim(&claims, 2, &held, &mut 5).unwrap();
        let carried_on = Claims::claim(&claims, 1, &held, &mut cursor).unwrap();
        assert_eq!(carried_on.index, 6);
    }
}

#[cfg(all(test, mux_loom))]
mod interleavings {
    use super::*;
    use loom::sync::atomic::{AtomicBool, Ordering};
    use loom::thread;

    const CHUNKS: u64 = 2;

    /// Claims a chunk and checks that nobody else holds it at the same time.
    fn claim_exclusively(
        claims: &Arc<Claims>,
        owner: Owner,
        held: &[AtomicBool],
        cursor: &mut u64,
    ) -> Option<Claim> {
        let claim = Claims::claim(claims, owner, &Bitfield::full(CHUNKS), cursor)?;
        let slot = usize::try_from(claim.index).unwrap();
        assert!(
            !held[slot].swap(true, Ordering::SeqCst),
            "chunk {slot} was claimed twice"
        );
        Some(claim)
    }

    fn let_go(held: &[AtomicBool], claim: &Claim) {
        held[usize::try_from(claim.index).unwrap()].store(false, Ordering::SeqCst);
    }

    /// One fetcher completes whatever it claims; the other gives its claim
    /// up. Under every interleaving, no chunk is held twice, and afterwards
    /// no chunk is left in flight.
    #[test]
    fn claims_are_exclusive_and_never_stranded() {
        loom::model(|| {
            let claims = Claims::new(&Bitfield::new(CHUNKS), || {});
            let held: Arc<Vec<AtomicBool>> =
                Arc::new((0..CHUNKS).map(|_| AtomicBool::new(false)).collect());

            let completer = thread::spawn({
                let (claims, held) = (claims.clone(), held.clone());
                move || {
                    let mut cursor = 0;
                    while let Some(claim) = claim_exclusively(&claims, 0, &held, &mut cursor) {
                        let_go(&held, &claim);
                        claims.complete(claim);
                    }
                }
            });
            let quitter = thread::spawn({
                let (claims, held) = (claims.clone(), held.clone());
                move || {
                    let mut cursor = 1;
                    if let Some(claim) = claim_exclusively(&claims, 1, &held, &mut cursor) {
                        let_go(&held, &claim);
                        drop(claim);
                    }
                }
            });
            completer.join().unwrap();
            quitter.join().unwrap();

            // Whatever the quitter put back after the completer had stopped
            // is still claimable, so the download can always be finished.
            let mut cursor = 0;
            while let Some(claim) = claim_exclusively(&claims, 0, &held, &mut cursor) {
                let_go(&held, &claim);
                claims.complete(claim);
            }
            let schedule = claims.schedule();
            assert!(schedule.is_complete());
            for index in 0..CHUNKS {
                assert_eq!(schedule.state(index), Some(ChunkState::Done));
            }
        });
    }
}
