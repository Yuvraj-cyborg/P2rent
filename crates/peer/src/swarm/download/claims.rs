//! The schedule behind a lock, handing out claims that put themselves back.
//!
//! This is the only part of a download that several tasks change at once, so
//! it is kept free of everything else and is checked under every
//! interleaving of two fetchers by the model test at the bottom.

use model::artifact::Bitfield;
use model::schedule::{Outcome, Owner, Schedule};
use std::sync::PoisonError;
use std::time::Instant;

// The model test swaps in lock and reference-count types that let it
// explore every interleaving.
#[cfg(mux_loom)]
use loom::sync::{Arc, Mutex, MutexGuard};
#[cfg(not(mux_loom))]
use std::sync::{Arc, Mutex, MutexGuard};

/// How the rest of the download holds the claims. Its type follows the
/// reference count in use, which differs under the model test.
pub(super) type Shared = Arc<Claims>;

struct State {
    schedule: Schedule,
    last_progress: Instant,
}

/// Which chunks are missing, being fetched and done, shared by every task
/// working on one download.
pub(super) struct Claims {
    state: Mutex<State>,
    /// Called when a chunk is put back or the last one completes, so that
    /// fetchers with nothing to do look again.
    wake: Box<dyn Fn() + Send + Sync>,
}

impl Claims {
    pub(super) fn new(have: &Bitfield, wake: impl Fn() + Send + Sync + 'static) -> Shared {
        Arc::new(Self {
            state: Mutex::new(State {
                schedule: Schedule::new(have),
                last_progress: Instant::now(),
            }),
            wake: Box::new(wake),
        })
    }

    // A poisoned lock is recovered: see `crate::sync`.
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(super) fn is_complete(&self) -> bool {
        self.state().schedule.is_complete()
    }

    /// When a chunk was last completed, or when the download started.
    pub(super) fn last_progress(&self) -> Instant {
        self.state().last_progress
    }

    /// Reserves for `owner` a missing chunk that `available` holds,
    /// searching from `cursor`. See [`Schedule::claim`].
    pub(super) fn claim(
        claims: &Shared,
        owner: Owner,
        available: &Bitfield,
        cursor: &mut u64,
    ) -> Option<Claim> {
        let index = claims.state().schedule.claim(owner, available, cursor)?;
        Some(Claim {
            claims: claims.clone(),
            index,
            owner,
            settled: false,
        })
    }

    /// Records a chunk that has been verified and stored.
    pub(super) fn complete(&self, mut claim: Claim) {
        claim.settled = true;
        let finished = {
            let mut state = self.state();
            state
                .schedule
                .settle(claim.index, claim.owner, Outcome::Verified);
            state.last_progress = Instant::now();
            state.schedule.is_complete()
        };
        if finished {
            (self.wake)();
        }
    }

    fn release(&self, index: u64, owner: Owner) {
        self.state().schedule.settle(index, owner, Outcome::Failed);
        (self.wake)();
    }
}

/// A reserved chunk. Dropping it without completing puts the chunk back, so
/// a cancelled or failed request never strands a chunk.
pub(super) struct Claim {
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

#[cfg(all(test, mux_loom))]
mod interleavings {
    use super::*;
    use loom::sync::atomic::{AtomicBool, Ordering};
    use loom::thread;
    use model::schedule::ChunkState;

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
            let state = claims.state();
            assert!(state.schedule.is_complete());
            for index in 0..CHUNKS {
                assert_eq!(state.schedule.state(index), Some(ChunkState::Done));
            }
        });
    }
}
