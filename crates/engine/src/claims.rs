//! The schedule behind a lock, handing out claims that put themselves back.
//!
//! This is the only part of a download that several tasks change at once, so
//! it is kept free of everything else and is checked under every
//! interleaving of two fetchers by the model test at the bottom.
//!
//! A fetcher reserves a run of chunks in a row and then takes them one at
//! a time. Where each request is a connection of its own, the run is what
//! one request asks for. The far end of what a fetcher has reserved and
//! not yet taken can be taken over by another that has nothing to do and
//! would get there sooner: see [`model::run`]. So can the chunk it is
//! reading. That one is then being fetched twice, and belongs to whoever
//! took it over: the first copy to come is stored, and the chunk is done
//! when its owner's has come.

use model::artifact::Bitfield;
use model::run;
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
    state: Mutex<State>,
    /// Called when a chunk is put back or the last one completes, so that
    /// fetchers with nothing to do look again.
    wake: Box<dyn Fn() + Send + Sync>,
}

struct State {
    schedule: Schedule,
    /// The runs that are being read, each under the number it was given.
    runs: Vec<(u64, Reading)>,
    /// The number the next run is given.
    numbers: u64,
}

/// What is known of a run that is being read.
struct Reading {
    owner: Owner,
    /// The chunks reserved and not yet taken: from `next` up to `end`.
    next: u64,
    end: u64,
    /// Whether another fetcher took over the end of the run.
    cut: bool,
    /// Whether the last chunk taken, the one before `next`, has been asked
    /// for and has not come, and is still this run's to fetch.
    fetching: bool,
    /// Whether the run is what one request asks for, and not a chunk that
    /// is a request of its own.
    stream: bool,
    /// How long a chunk takes whoever reads the run, in microseconds, or
    /// zero if it has fetched none yet.
    micros_each: u64,
    /// When the last chunk came or, if none has, when the run was begun:
    /// microseconds since the transfer started.
    since: u64,
}

impl Reading {
    /// How many chunks are reserved and not yet taken.
    fn left(&self) -> u64 {
        self.end - self.next
    }

    /// How long a chunk of the run is reckoned to take, at time `now`:
    /// what one lately took. But a run that has brought nothing for longer
    /// than is worth a request is reckoned at how long that has been, so
    /// that one that has stopped coming is given up in the end, however
    /// fast it once was.
    fn each(&self, now: u64) -> u64 {
        let awaited = now.saturating_sub(self.since);
        if awaited > run::WORTH_MICROS {
            self.micros_each.max(awaited)
        } else {
            self.micros_each
        }
    }

    /// How many chunks to give up from the end, at time `now`, to a
    /// fetcher that takes `mine` for a chunk: see [`run::spare`].
    fn spare(&self, now: u64, mine: u64) -> u64 {
        run::spare(self.left(), self.each(now), mine)
    }

    /// The chunk that is being read, if a fetcher that takes `mine` for a
    /// chunk should fetch it as well at time `now`, and how long that is
    /// reckoned to save: see [`run::relieve`].
    fn relief(&self, now: u64, mine: u64) -> Option<(u64, u64)> {
        let waited = now.saturating_sub(self.since);
        let each = self.each(now);
        let relieved = self.fetching && run::relieve(each, waited, mine);
        relieved.then(|| (waited.max(each), self.next - 1))
    }
}

/// How many chunks a fetcher reserves at once.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Size {
    /// One: each chunk is a request of its own.
    One,
    /// What one request is to ask for, when `streams` fetchers are taking
    /// runs and a chunk has lately taken this one `micros_each`: its share
    /// by speed of the work that is left, see [`run::length`].
    Run {
        /// How many fetchers are taking runs.
        streams: u64,
        /// How long a chunk lately took this one, in microseconds.
        micros_each: u64,
    },
}

impl Claims {
    pub(crate) fn new(have: &Bitfield, wake: impl Fn() + Send + Sync + 'static) -> Shared {
        Arc::new(Self {
            state: Mutex::new(State {
                schedule: Schedule::new(have),
                runs: Vec::new(),
                numbers: 0,
            }),
            wake: Box::new(wake),
        })
    }

    // A poisoned lock is recovered: see `crate::sync`.
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn is_complete(&self) -> bool {
        self.state().schedule.is_complete()
    }

    /// Whether the supplier fetching as `owner`, which holds `available`,
    /// has work: a chunk it is fetching or has reserved, or one it holds
    /// that is missing and that nobody is fetching.
    pub(crate) fn has_work_for(&self, owner: Owner, available: &Bitfield) -> bool {
        let state = self.state();
        let schedule = &state.schedule;
        (0..schedule.len()).any(|index| match schedule.state(index) {
            Some(ChunkState::Missing) => available.get(index),
            Some(ChunkState::InFlight(by)) => by == owner,
            Some(ChunkState::Done) | None => false,
        })
    }

    /// Whether any chunk is reserved: waiting to be asked for, on its way,
    /// or arrived and being stored.
    pub(crate) fn any_in_flight(&self) -> bool {
        let state = self.state();
        let schedule = &state.schedule;
        (0..schedule.len())
            .any(|index| matches!(schedule.state(index), Some(ChunkState::InFlight(_))))
    }

    /// Whether any chunk is missing with nobody fetching it.
    #[cfg(all(test, not(mux_loom)))]
    fn any_missing(&self) -> bool {
        let state = self.state();
        let schedule = &state.schedule;
        (0..schedule.len()).any(|index| schedule.state(index) == Some(ChunkState::Missing))
    }

    /// Reserves for `owner` a run of missing chunks that `available` holds:
    /// the first such chunk from `cursor` on (see [`Schedule::claim`]) and
    /// as many of those right after it as `size` allows. The cursor moves
    /// past the run. `now` is the time, in microseconds since the transfer
    /// started.
    pub(crate) fn reserve(
        claims: &Shared,
        owner: Owner,
        available: &Bitfield,
        cursor: &mut u64,
        size: Size,
        now: u64,
    ) -> Option<Reserved> {
        let mut state = claims.state();
        let most = match size {
            Size::One => 1,
            Size::Run {
                streams,
                micros_each,
            } => {
                let schedule = &state.schedule;
                let free = (0..schedule.len())
                    .filter(|&index| is_free(schedule, available, index))
                    .count() as u64;
                // The work that is left, and how fast the streams are
                // that will do it. Each stream that is reading a run is
                // taken to have a chunk on its way besides those it has
                // still to ask for. One whose speed is not known, or that
                // is between two runs as this one is, is taken to be as
                // fast as this one.
                let reading = state.runs.iter().filter(|(_, reading)| reading.stream);
                let (mut work, mut speeds, mut known) = (free, 0u64, 0u64);
                for (_, reading) in reading {
                    let each = match reading.micros_each {
                        0 => micros_each,
                        each => each,
                    };
                    work += reading.left() + 1;
                    speeds = speeds.saturating_add(run::speed(each));
                    known += 1;
                }
                let between = streams.saturating_sub(known).max(1);
                speeds = speeds.saturating_add(run::speed(micros_each).saturating_mul(between));
                run::length(free, streams, micros_each, run::micros_left(work, speeds))
            }
        };
        let lately = match size {
            Size::One => 0,
            Size::Run { micros_each, .. } => micros_each,
        };
        let first = state.schedule.claim(owner, available, cursor)?;
        let mut end = first + 1;
        while end - first < most && is_free(&state.schedule, available, end) {
            // The cursor is at `end`, which is free: that is the chunk
            // this takes.
            state.schedule.claim(owner, available, cursor);
            end += 1;
        }
        let number = state.numbers;
        state.numbers += 1;
        state.runs.push((
            number,
            Reading {
                owner,
                next: first,
                end,
                cut: false,
                fetching: false,
                stream: matches!(size, Size::Run { .. }),
                micros_each: lately,
                since: now,
            },
        ));
        drop(state);
        Some(Reserved {
            claims: claims.clone(),
            number,
        })
    }

    /// Takes over, for `owner`, a chunk that another fetcher is reading:
    /// one that `available` holds and that `owner`, which takes `mine`
    /// microseconds for a chunk, would have sooner by enough
    /// ([`run::relieve`]). Of those, the one that saves most time. For a
    /// fetcher that has nothing free to reserve and nothing to take over
    /// from the end of a run. `now` is the time, in microseconds since the
    /// transfer started.
    ///
    /// The chunk is `owner`'s from here on. The fetcher that was reading
    /// it is not stopped: if its copy comes first, that is the one stored,
    /// and nothing more comes of the other.
    pub(crate) fn relieve(
        claims: &Shared,
        owner: Owner,
        available: &Bitfield,
        now: u64,
        mine: u64,
    ) -> Option<Reserved> {
        let mut state = claims.state();
        let best = state
            .runs
            .iter()
            .enumerate()
            .filter(|(_, (_, reading))| reading.owner != owner)
            .filter_map(|(place, (_, reading))| {
                let (saved, index) = reading.relief(now, mine)?;
                available.get(index).then_some((saved, place, index))
            })
            .max();
        let (_, place, index) = best?;
        let reading = &mut state.runs[place].1;
        let was = reading.owner;
        reading.fetching = false;
        if !state.schedule.settle(index, was, Outcome::Failed) {
            return None;
        }
        // The chunk is missing for this instant only, and the cursor is on
        // it: it is the one this claims.
        let mut cursor = index;
        let claimed = state.schedule.claim(owner, available, &mut cursor);
        debug_assert_eq!(claimed, Some(index));
        claimed?;
        let number = state.numbers;
        state.numbers += 1;
        state.runs.push((
            number,
            Reading {
                owner,
                next: index,
                end: index + 1,
                cut: false,
                fetching: false,
                stream: false,
                micros_each: 0,
                since: now,
            },
        ));
        drop(state);
        Some(Reserved {
            claims: claims.clone(),
            number,
        })
    }

    /// Puts back the far end of a run for a fetcher that has nothing to do
    /// and takes `mine` microseconds for a chunk (zero if it has fetched
    /// none): as much as it could fetch by the time the run's reader would
    /// get there, if that is worth cutting the reader off for
    /// ([`run::spare`]) and `available` holds it. Of the runs that
    /// qualify, the one that gains most time. Whoever has nothing to do
    /// can then reserve what was put back. `now` is the time, in
    /// microseconds since the transfer started. Returns whether anything
    /// was put back.
    pub(crate) fn spare(&self, available: &Bitfield, now: u64, mine: u64) -> bool {
        let mut state = self.state();
        let best = state
            .runs
            .iter()
            .enumerate()
            .filter_map(|(place, (_, reading))| {
                let spared = reading.spare(now, mine);
                let from = reading.end - spared;
                let held = (from..reading.end).all(|index| available.get(index));
                let gained = spared.saturating_mul(reading.each(now));
                (spared > 0 && held).then_some((gained, place, from))
            })
            .max();
        let Some((_, place, from)) = best else {
            return false;
        };
        let reading = &mut state.runs[place].1;
        let (owner, end) = (reading.owner, reading.end);
        reading.end = from;
        reading.cut = true;
        for index in from..end {
            state.schedule.settle(index, owner, Outcome::Failed);
        }
        drop(state);
        (self.wake)();
        true
    }

    /// Records a chunk that has been verified and stored.
    pub(crate) fn complete(&self, mut claim: Claim) {
        claim.settled = true;
        let finished = {
            let mut state = self.state();
            state
                .schedule
                .settle(claim.index, claim.owner, Outcome::Verified);
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

/// Whether chunk `index` is missing, with nobody fetching it, and
/// `available` holds it.
fn is_free(schedule: &Schedule, available: &Bitfield, index: u64) -> bool {
    schedule.state(index) == Some(ChunkState::Missing) && available.get(index)
}

/// A run of chunks reserved for one fetcher, which takes them in order.
/// Dropping it puts back those it has not taken.
pub(crate) struct Reserved {
    claims: Shared,
    number: u64,
}

/// What comes next of a run.
pub(crate) enum Next {
    /// A chunk, and how many of the run are to follow it.
    Chunk(Claim, u64),
    /// Nothing: every chunk of the run has been taken.
    Over,
    /// Nothing: another fetcher took over the rest, from this chunk on.
    Cut(u64),
}

impl Reserved {
    /// Takes the next chunk of the run.
    pub(crate) fn next(&mut self) -> Next {
        let mut state = self.claims.state();
        let Some((_, reading)) = state.runs.iter_mut().find(|(n, _)| *n == self.number) else {
            return Next::Over;
        };
        if reading.next == reading.end {
            return if reading.cut {
                Next::Cut(reading.next)
            } else {
                Next::Over
            };
        }
        let index = reading.next;
        reading.next += 1;
        reading.fetching = true;
        let (owner, following) = (reading.owner, reading.left());
        drop(state);
        let claim = Claim {
            claims: self.claims.clone(),
            index,
            owner,
            settled: false,
        };
        Next::Chunk(claim, following)
    }

    /// Takes in that a chunk of the run has come, at time `now`, and took
    /// `micros_each`.
    ///
    /// The first time, fetchers with nothing to do are told to look again:
    /// nothing is taken over from a run of which nothing is known, and now
    /// something is. Left to find out by themselves they would look in
    /// their own time, and a transfer would end that much later.
    pub(crate) fn learn(&self, micros_each: u64, now: u64) {
        let first = {
            let mut state = self.claims.state();
            let reading = state.runs.iter_mut().find(|(n, _)| *n == self.number);
            reading.is_some_and(|(_, reading)| {
                let first = reading.micros_each == 0 && micros_each > 0;
                reading.micros_each = micros_each;
                reading.since = now;
                reading.fetching = false;
                first
            })
        };
        if first {
            (self.claims.wake)();
        }
    }
}

impl Reserved {
    /// Takes in how long a chunk of the run looks like taking, going by
    /// how much of its first has come: `micros_each`. It counts only
    /// until the first chunk has come whole, and from then on what
    /// [`Reserved::learn`] is told does. A run whose first chunk is slow
    /// is then known to be slow before that chunk is there, and fetchers
    /// with nothing to do are told, as when it is first learned.
    pub(crate) fn reckon(&self, micros_each: u64) {
        let first = {
            let mut state = self.claims.state();
            let reading = state.runs.iter_mut().find(|(n, _)| *n == self.number);
            reading.is_some_and(|(_, reading)| {
                let first = reading.micros_each == 0 && micros_each > 0;
                if first {
                    reading.micros_each = micros_each;
                }
                first
            })
        };
        if first {
            (self.claims.wake)();
        }
    }
}

impl Drop for Reserved {
    fn drop(&mut self) {
        let mut state = self.claims.state();
        let Some(place) = state.runs.iter().position(|(n, _)| *n == self.number) else {
            return;
        };
        let (_, reading) = state.runs.swap_remove(place);
        for index in reading.next..reading.end {
            state.schedule.settle(index, reading.owner, Outcome::Failed);
        }
        drop(state);
        if reading.next < reading.end {
            (self.claims.wake)();
        }
    }
}

/// A chunk that is being fetched. Dropping it without completing puts the
/// chunk back, so a cancelled or failed request never strands a chunk.
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
    use super::ChunkState::{InFlight, Missing};
    use super::*;

    /// A millisecond, in the microseconds that times are counted in.
    const MS: u64 = 1_000;

    /// Reserves one chunk and takes it.
    fn one(claims: &Shared, owner: Owner, held: &Bitfield, cursor: &mut u64) -> Option<Claim> {
        let mut reserved = Claims::reserve(claims, owner, held, cursor, Size::One, 0)?;
        match reserved.next() {
            Next::Chunk(claim, 0) => Some(claim),
            _ => panic!("a run of one chunk has one chunk and nothing after it"),
        }
    }

    /// A run for one of `streams` fetchers that has fetched nothing yet.
    fn run(streams: u64) -> Size {
        Size::Run {
            streams,
            micros_each: 0,
        }
    }

    /// The chunks of a run and what was to follow each, taken to its end.
    fn taken(reserved: &mut Reserved) -> (Vec<(u64, u64)>, Vec<Claim>) {
        let (mut seen, mut claims) = (Vec::new(), Vec::new());
        while let Next::Chunk(claim, following) = reserved.next() {
            seen.push((claim.index, following));
            claims.push(claim);
        }
        (seen, claims)
    }

    fn states(claims: &Claims) -> Vec<ChunkState> {
        let state = claims.state();
        (0..state.schedule.len())
            .map(|index| state.schedule.state(index).unwrap())
            .collect()
    }

    #[test]
    fn a_chunk_is_in_flight_from_its_claim_until_it_is_settled() {
        let claims = Claims::new(&Bitfield::new(2), || {});
        let held = Bitfield::full(2);
        assert!(!claims.any_in_flight());

        let kept = one(&claims, 1, &held, &mut 0).unwrap();
        let let_go = one(&claims, 1, &held, &mut 0).unwrap();
        assert!(claims.any_in_flight());
        drop(let_go);
        assert!(claims.any_in_flight());
        claims.complete(kept);
        assert!(!claims.any_in_flight());
        assert!(!claims.is_complete());
    }

    #[test]
    fn a_run_is_reserved_whole_and_taken_in_order() {
        let claims = Claims::new(&Bitfield::new(64), || {});
        let held = Bitfield::full(64);
        let mut cursor = 0;
        // Half of a share of what is free: 64 over two times four.
        let mut first = Claims::reserve(&claims, 1, &held, &mut cursor, run(4), 0).unwrap();
        assert_eq!(cursor, 8);
        assert!(
            states(&claims)[..8]
                .iter()
                .all(|&state| state == ChunkState::InFlight(1))
        );
        assert_eq!(states(&claims)[8], ChunkState::Missing);

        // The next run follows on, and is a share of what is left: 56 over
        // eight.
        let mut second = Claims::reserve(&claims, 2, &held, &mut cursor, run(4), 0).unwrap();
        assert_eq!(cursor, 15);

        let (seen, _held) = taken(&mut first);
        let expected: Vec<(u64, u64)> = (0..8).map(|index| (index, 7 - index)).collect();
        assert_eq!(seen, expected);
        assert!(matches!(first.next(), Next::Over));
        let (seen, _held) = taken(&mut second);
        assert_eq!(seen.first(), Some(&(8, 6)));
        assert_eq!(seen.last(), Some(&(14, 0)));
    }

    /// Two streams take their first runs. The second fetches all of its
    /// own, at 100 ms a chunk, while the first has taken one chunk. Returns
    /// how many chunks the second then takes, when the first is known to
    /// take `first_each` for a chunk.
    fn second_run(chunks: u64, first_each: u64) -> usize {
        let claims = Claims::new(&Bitfield::new(chunks), || {});
        let held = Bitfield::full(chunks);
        let mut cursor = 0;
        let mut first = Claims::reserve(&claims, 1, &held, &mut cursor, run(2), 0).unwrap();
        let mut second = Claims::reserve(&claims, 1, &held, &mut cursor, run(2), 0).unwrap();
        let Next::Chunk(_fetching, _) = first.next() else {
            panic!("the first run has chunks");
        };
        first.learn(first_each, 0);
        while let Next::Chunk(claim, _) = second.next() {
            claims.complete(claim);
        }
        drop(second);
        let size = Size::Run {
            streams: 2,
            micros_each: 100 * MS,
        };
        let mut again = Claims::reserve(&claims, 1, &held, &mut cursor, size, 0).unwrap();
        taken(&mut again).0.len()
    }

    /// A later run is the stream's share by speed of the work that is
    /// left: what is free, what the others have still to ask for, and a
    /// chunk for each of them that is on its way.
    #[test]
    fn a_later_run_is_a_share_by_speed_of_the_work_that_is_left() {
        // Forty chunks: first runs of ten and seven, so 23 are free. The
        // first has nine still to ask for and one on its way: 33 chunks
        // of work for two streams. Nothing is known of the first, so it is
        // taken to be as fast, and half of 33 is 17 when rounded up.
        assert_eq!(second_run(40, 0), 17);
        // A hundred chunks: first runs of 25 and 18, so 57 are free and
        // there are 82 chunks of work. A first stream that is twice as
        // fast leaves the second a third of them.
        assert_eq!(second_run(100, 50 * MS), 28);
        // One that is ten times slower leaves it all that is free.
        assert_eq!(second_run(100, 1_000 * MS), 57);
    }

    #[test]
    fn a_stream_by_itself_reserves_everything_in_a_row() {
        let mut have = Bitfield::new(10);
        have.set(6);
        let claims = Claims::new(&have, || {});
        let held = Bitfield::full(10);
        let mut cursor = 0;
        // Up to the chunk that is already held, and then from past it.
        let mut first = Claims::reserve(&claims, 1, &held, &mut cursor, run(1), 0).unwrap();
        let (seen, _fetching) = taken(&mut first);
        assert_eq!(seen.len(), 6);
        let mut second = Claims::reserve(&claims, 1, &held, &mut cursor, run(1), 0).unwrap();
        let (seen, _fetching_too) = taken(&mut second);
        assert_eq!(seen, [(7, 2), (8, 1), (9, 0)]);
        assert!(Claims::reserve(&claims, 1, &held, &mut cursor, run(1), 0).is_none());
    }

    #[test]
    fn a_run_stops_at_a_chunk_the_supplier_does_not_hold() {
        let claims = Claims::new(&Bitfield::new(8), || {});
        let mut held = Bitfield::full(8);
        held.clear(3);
        let mut cursor = 1;
        let mut reserved = Claims::reserve(&claims, 1, &held, &mut cursor, run(1), 0).unwrap();
        let (seen, _first) = taken(&mut reserved);
        assert_eq!(seen, [(1, 1), (2, 0)]);
        assert_eq!(cursor, 3);
        // A run does not go round the end, and the search for the next
        // does.
        let mut cursor = 7;
        let mut reserved = Claims::reserve(&claims, 1, &held, &mut cursor, run(1), 0).unwrap();
        let (seen, _second) = taken(&mut reserved);
        assert_eq!(seen, [(7, 0)]);
        assert_eq!(cursor, 0);
        let mut reserved = Claims::reserve(&claims, 1, &held, &mut cursor, run(1), 0).unwrap();
        let (seen, _third) = taken(&mut reserved);
        assert_eq!(seen, [(0, 0)]);
    }

    #[test]
    fn what_is_not_taken_of_a_run_is_put_back_when_it_is_dropped() {
        let woken = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let claims = Claims::new(&Bitfield::new(4), {
            let woken = woken.clone();
            move || {
                woken.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        });
        let held = Bitfield::full(4);
        let mut reserved = Claims::reserve(&claims, 1, &held, &mut 0, run(1), 0).unwrap();
        let Next::Chunk(fetching, 3) = reserved.next() else {
            panic!("the run has four chunks");
        };
        drop(reserved);
        // The chunk that was taken is still being fetched.
        assert_eq!(states(&claims), [InFlight(1), Missing, Missing, Missing]);
        assert_eq!(woken.load(std::sync::atomic::Ordering::Relaxed), 1);
        claims.complete(fetching);
        assert_eq!(states(&claims)[0], ChunkState::Done);

        // A run that was taken to its end puts nothing back and wakes
        // nobody.
        let mut reserved = Claims::reserve(&claims, 1, &held, &mut 0, run(1), 0).unwrap();
        let (_, fetching) = taken(&mut reserved);
        drop(reserved);
        assert_eq!(woken.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert!(claims.any_in_flight());
        drop(fetching);
        assert!(!claims.any_in_flight());
    }

    #[test]
    fn the_far_end_of_a_run_is_put_back_for_one_that_can_fetch_it_sooner() {
        let claims = Claims::new(&Bitfield::new(16), || {});
        let held = Bitfield::full(16);
        let mut slow = Claims::reserve(&claims, 1, &held, &mut 0, run(1), 0).unwrap();
        let Next::Chunk(_fetching, 15) = slow.next() else {
            panic!("the run has sixteen chunks");
        };
        // Nothing is known of it yet, and it has only just begun.
        assert!(!claims.spare(&held, 0, 0));
        // A chunk came, and took half a second. A fetcher that is as fast
        // takes half of the fifteen that are left, rounded up.
        slow.learn(500 * MS, 500 * MS);
        assert!(claims.spare(&held, 500 * MS, 500 * MS));
        let after = states(&claims);
        assert!(after[..8].iter().all(|&state| state == InFlight(1)));
        assert!(after[8..].iter().all(|&state| state == Missing));

        // That half is taken by whoever had nothing to do.
        let mut quick = Claims::reserve(&claims, 2, &held, &mut 0, run(1), 500 * MS).unwrap();
        assert_eq!(taken(&mut quick).0.len(), 8);
        // Seven are left to the slow one. Half of them would bring its end
        // forward by two seconds, which is not worth cutting it off for.
        assert!(!claims.spare(&held, 500 * MS, 500 * MS));
        // A fetcher five times as fast would take six of them, and that is.
        assert!(claims.spare(&held, 500 * MS, 100 * MS));
        let (seen, _fetching) = taken(&mut slow);
        assert_eq!(seen, [(1, 0)]);
        let Next::Cut(2) = slow.next() else {
            panic!("the run was cut at chunk 2");
        };
    }

    #[test]
    fn a_run_that_has_stopped_coming_is_given_up_in_time() {
        let claims = Claims::new(&Bitfield::new(3), || {});
        let held = Bitfield::full(3);
        let mut stuck = Claims::reserve(&claims, 1, &held, &mut 0, run(1), 1_000 * MS).unwrap();
        let Next::Chunk(_fetching, 2) = stuck.next() else {
            panic!("the run has three chunks");
        };
        // Its first chunk has been awaited for two seconds, which a chunk
        // may take. Nothing says yet how long the two after it will.
        let quick = 100 * MS;
        assert!(!claims.spare(&held, 3_000 * MS, quick));
        // Awaited longer than that, it is reckoned at how long it has
        // been: a fetcher that takes a tenth of a second for a chunk could
        // fetch both of the others before this one came.
        assert!(claims.spare(&held, 3_000 * MS + 1, quick));
        assert!(!claims.spare(&held, 3_000 * MS + 1, quick));
        assert_eq!(states(&claims), [InFlight(1), Missing, Missing]);
        let Next::Cut(1) = stuck.next() else {
            panic!("nothing is left of the run");
        };
    }

    /// Nothing is taken over from a run of which nothing is known. So when
    /// the first chunk of one has come, whoever has nothing to do is told:
    /// there may be something to take over now.
    #[test]
    fn fetchers_with_nothing_to_do_are_told_when_a_run_first_learns_its_speed() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let told = std::sync::Arc::new(AtomicUsize::new(0));
        let counting = told.clone();
        let claims = Claims::new(&Bitfield::new(8), move || {
            counting.fetch_add(1, Ordering::Relaxed);
        });
        let held = Bitfield::full(8);
        let mut slow = Claims::reserve(&claims, 1, &held, &mut 0, run(1), 0).unwrap();
        let Next::Chunk(_fetching, 7) = slow.next() else {
            panic!("the run has eight chunks");
        };
        // A fetcher a hundred times faster may take nothing yet.
        let quick = 3 * MS;
        assert!(!claims.spare(&held, 300 * MS, quick));
        assert_eq!(told.load(Ordering::Relaxed), 0);

        slow.learn(300 * MS, 300 * MS);
        assert_eq!(told.load(Ordering::Relaxed), 1);
        // Now it takes all seven that are not being read: it would have
        // them in a fiftieth of a second, and the run in two.
        assert!(claims.spare(&held, 300 * MS, quick));
        assert_eq!(states(&claims).iter().filter(|&&s| s == Missing).count(), 7);
        // Told of that too, and of nothing more when the next chunk comes.
        let before = told.load(Ordering::Relaxed);
        slow.learn(280 * MS, 600 * MS);
        assert_eq!(told.load(Ordering::Relaxed), before);
    }

    /// A chunk that is being read is taken over by a fetcher that would
    /// have it far sooner. It is then the taker's: done when the taker's
    /// copy is stored, whatever becomes of the reader's.
    #[test]
    fn a_chunk_that_is_being_read_is_taken_over_by_a_much_faster_fetcher() {
        let claims = Claims::new(&Bitfield::new(2), || {});
        let held = Bitfield::full(2);
        let mut slow = Claims::reserve(&claims, 1, &held, &mut 0, run(1), 0).unwrap();
        let quick = 10 * MS;
        // Nothing is being read yet.
        assert!(Claims::relieve(&claims, 2, &held, 500 * MS, quick).is_none());
        let Next::Chunk(reading, 1) = slow.next() else {
            panic!("the run has two chunks");
        };
        // Nor is anything known of the reader, half a second in.
        assert!(Claims::relieve(&claims, 2, &held, 500 * MS, quick).is_none());
        // By what has come of the chunk, it looks like taking eight
        // seconds. Its own supplier leaves it, and so does a fetcher that
        // does not hold it. One that is quick takes it.
        slow.reckon(8_000 * MS);
        assert!(Claims::relieve(&claims, 1, &held, 500 * MS, quick).is_none());
        assert!(Claims::relieve(&claims, 2, &Bitfield::new(2), 500 * MS, quick).is_none());
        let mut taken = Claims::relieve(&claims, 2, &held, 500 * MS, quick).unwrap();
        assert_eq!(states(&claims), [InFlight(2), InFlight(1)]);
        // Once, and not again from the one who took it.
        assert!(Claims::relieve(&claims, 3, &held, 500 * MS, quick).is_none());
        let Next::Chunk(mine, 0) = taken.next() else {
            panic!("the chunk that was taken over is there to take");
        };
        assert_eq!(mine.index, 0);

        // The reader's copy comes first. The chunk is not the reader's to
        // settle, and is done when the taker's has come.
        claims.complete(reading);
        assert_eq!(states(&claims), [InFlight(2), InFlight(1)]);
        claims.complete(mine);
        assert_eq!(states(&claims)[0], ChunkState::Done);
        // The rest of the reader's run is still its own.
        let Next::Chunk(next, 0) = slow.next() else {
            panic!("one chunk of the run is left");
        };
        assert_eq!(next.index, 1);
    }

    /// The chunk that is taken over is the one that saves most time.
    #[test]
    fn of_two_chunks_being_read_the_one_longest_in_coming_is_taken_over() {
        let claims = Claims::new(&Bitfield::new(2), || {});
        let held = Bitfield::full(2);
        let mut cursor = 0;
        let mut early = Claims::reserve(&claims, 1, &held, &mut cursor, Size::One, 0).unwrap();
        let mut late =
            Claims::reserve(&claims, 2, &held, &mut cursor, Size::One, 4_000 * MS).unwrap();
        let (Next::Chunk(_first, 0), Next::Chunk(_second, 0)) = (early.next(), late.next()) else {
            panic!("each run has its chunk");
        };
        // Neither has brought anything. One has been awaited for five
        // seconds, the other for one: only the first is reckoned stuck.
        let taken = Claims::relieve(&claims, 3, &held, 5_000 * MS, 10 * MS).unwrap();
        drop(taken);
        assert_eq!(states(&claims), [Missing, InFlight(2)]);
    }

    #[test]
    fn the_run_that_gains_most_time_is_the_one_cut() {
        let claims = Claims::new(&Bitfield::new(40), || {});
        let held = Bitfield::full(40);
        let mut cursor = 0;
        let quick = Claims::reserve(&claims, 1, &held, &mut cursor, run(2), 0).unwrap();
        let slow = Claims::reserve(&claims, 2, &held, &mut cursor, run(2), 0).unwrap();
        // Ten chunks at 300 ms, and seven at a second. A fetcher that
        // takes 300 ms could take five of the first, which saves a second
        // and a half, or six of the second, which saves six.
        quick.learn(300 * MS, 1_000 * MS);
        slow.learn(1_000 * MS, 1_000 * MS);
        assert!(claims.spare(&held, 1_000 * MS, 300 * MS));
        let after = states(&claims);
        assert!(after[..10].iter().all(|&state| state == InFlight(1)));
        assert_eq!(after[10], InFlight(2));
        assert!(after[11..17].iter().all(|&state| state == Missing));
    }

    #[test]
    fn a_slow_fetcher_does_not_cut_a_fast_one_off() {
        let claims = Claims::new(&Bitfield::new(40), || {});
        let held = Bitfield::full(40);
        // Forty chunks at 125 ms: five seconds are left of it.
        let fast = Claims::reserve(&claims, 1, &held, &mut 0, run(1), 0).unwrap();
        fast.learn(125 * MS, 1_000 * MS);
        // One that takes a second for a chunk could fetch five of them in
        // that time, and would save well under a second.
        assert!(!claims.spare(&held, 1_000 * MS, 1_000 * MS));
        assert!(!claims.any_missing());
        // One as fast takes half.
        assert!(claims.spare(&held, 1_000 * MS, 125 * MS));
    }

    #[test]
    fn nothing_is_put_back_for_a_supplier_that_does_not_hold_it() {
        let claims = Claims::new(&Bitfield::new(8), || {});
        let held = Bitfield::full(8);
        let slow = Claims::reserve(&claims, 1, &held, &mut 0, run(1), 0).unwrap();
        slow.learn(1_000 * MS, 1_000 * MS);
        let mut lacking = Bitfield::full(8);
        lacking.clear(6);
        assert!(!claims.spare(&lacking, 1_000 * MS, 0));
        assert!(!claims.any_missing());
        assert!(claims.spare(&held, 1_000 * MS, 0));
        assert!(claims.any_missing());
    }

    /// Fetchers of unequal speed, each reading runs with a request of its
    /// own. Time goes in steps of a millisecond. A fetcher needs
    /// `round_trip` before the first chunk of a run begins to come, and
    /// then `each[fetcher]` for every chunk. Returns how long the whole
    /// took, how many runs were begun and how many were cut.
    fn copy(chunks: u64, each: &[u64], round_trip: u64) -> (u64, usize, usize) {
        struct Fetcher {
            reserved: Option<Reserved>,
            fetching: Option<Claim>,
            /// When the chunk being fetched is there.
            due: u64,
            lately: u64,
        }
        let claims = Claims::new(&Bitfield::new(chunks), || {});
        let held = Bitfield::full(chunks);
        let streams = each.len() as u64;
        let mut cursor = 0;
        let mut fetchers: Vec<Fetcher> = each
            .iter()
            .map(|_| Fetcher {
                reserved: None,
                fetching: None,
                due: 0,
                lately: 0,
            })
            .collect();
        let (mut now, mut runs, mut cuts) = (0, 0, 0);
        while !claims.is_complete() {
            for (fetcher, &millis) in fetchers.iter_mut().zip(each) {
                if fetcher.fetching.is_some() && now < fetcher.due {
                    continue;
                }
                if let Some(done) = fetcher.fetching.take() {
                    claims.complete(done);
                    fetcher.lately = millis;
                    if let Some(reserved) = &fetcher.reserved {
                        reserved.learn(millis * MS, now * MS);
                    }
                }
                let mut began = false;
                if fetcher.reserved.is_none() {
                    let size = Size::Run {
                        streams,
                        micros_each: fetcher.lately * MS,
                    };
                    let (owner, at) = (1, now * MS);
                    let mut reserved =
                        Claims::reserve(&claims, owner, &held, &mut cursor, size, at);
                    if reserved.is_none() && claims.spare(&held, at, fetcher.lately * MS) {
                        reserved = Claims::reserve(&claims, owner, &held, &mut cursor, size, at);
                    }
                    fetcher.reserved = reserved;
                    began = fetcher.reserved.is_some();
                    runs += usize::from(began);
                }
                let Some(reserved) = &mut fetcher.reserved else {
                    continue;
                };
                match reserved.next() {
                    Next::Chunk(claim, _) => {
                        fetcher.fetching = Some(claim);
                        fetcher.due = now + millis + if began { round_trip } else { 0 };
                    }
                    Next::Over => fetcher.reserved = None,
                    Next::Cut(_) => {
                        fetcher.reserved = None;
                        cuts += 1;
                    }
                }
            }
            now += 1;
        }
        (now, runs, cuts)
    }

    /// How long `chunks` chunks would take fetchers that never stood idle,
    /// in milliseconds.
    fn at_best(chunks: u64, each: &[u64]) -> u64 {
        // Chunks in a thousand seconds, by all of them together.
        let speeds: u64 = each.iter().map(|&millis| 1_000_000 / millis).sum();
        chunks * 1_000_000 / speeds
    }

    /// Four streams of the same speed, far from the server: they end
    /// together, with few requests and none cut off.
    #[test]
    fn streams_of_one_speed_end_together_and_none_is_cut() {
        let each = [330; 4];
        let (took, runs, cuts) = copy(128, &each, 200);
        let best = at_best(128, &each);
        assert!(took < best + best / 10, "took {took} ms against {best}");
        assert!(runs <= 24, "{runs} requests");
        assert_eq!(cuts, 0);
    }

    /// One stream is three times slower than the others. What it would
    /// have held up is taken over, and the whole ends near the best there
    /// could be.
    #[test]
    fn a_slow_stream_does_not_hold_the_others_up() {
        let each = [250, 250, 250, 750];
        let (took, runs, _) = copy(256, &each, 100);
        let best = at_best(256, &each);
        assert!(took < best + best / 8, "took {took} ms against {best}");
        assert!(runs <= 40, "{runs} requests");
    }

    /// One stream all but stops. The others finish the copy without it.
    #[test]
    fn a_stream_that_stops_is_done_without() {
        // The chunk it is fetching when it stops is its own until it comes,
        // five seconds on. Everything else is fetched by the other three,
        // at ten chunks a second each.
        let each = [100, 100, 100, 5_000];
        let (took, _, cuts) = copy(64, &each, 50);
        assert!(took < 5_200, "took {took} ms");
        assert!(cuts >= 1);
    }

    /// A stream by itself makes one request.
    #[test]
    fn a_stream_by_itself_makes_one_request() {
        let (took, runs, cuts) = copy(100, &[10], 300);
        assert_eq!((took, runs, cuts), (1_301, 1, 0));
    }
}

#[cfg(all(test, mux_loom))]
mod interleavings {
    use super::*;
    use loom::sync::atomic::{AtomicBool, Ordering};
    use loom::thread;

    const CHUNKS: u64 = 2;

    /// Marks a chunk as taken and checks that nobody else has it at the
    /// same time.
    fn take_exclusively(held: &[AtomicBool], claim: &Claim) {
        let slot = usize::try_from(claim.index).unwrap();
        assert!(
            !held[slot].swap(true, Ordering::SeqCst),
            "chunk {slot} was claimed twice"
        );
    }

    fn let_go(held: &[AtomicBool], claim: &Claim) {
        held[usize::try_from(claim.index).unwrap()].store(false, Ordering::SeqCst);
    }

    /// Reserves one chunk and takes it.
    fn claim_exclusively(
        claims: &Shared,
        owner: Owner,
        held: &[AtomicBool],
        cursor: &mut u64,
    ) -> Option<Claim> {
        let all = Bitfield::full(CHUNKS);
        let mut reserved = Claims::reserve(claims, owner, &all, cursor, Size::One, 0)?;
        let Next::Chunk(claim, _) = reserved.next() else {
            panic!("a run that was reserved has a chunk");
        };
        take_exclusively(held, &claim);
        Some(claim)
    }

    /// Completes everything that can still be had, and checks that this is
    /// everything: no chunk was left reserved by anybody.
    fn finish(claims: &Shared, held: &[AtomicBool]) {
        let mut cursor = 0;
        while let Some(claim) = claim_exclusively(claims, 0, held, &mut cursor) {
            let_go(held, &claim);
            claims.complete(claim);
        }
        let state = claims.state();
        assert!(state.schedule.is_complete());
        assert!(state.runs.is_empty());
        for index in 0..CHUNKS {
            assert_eq!(state.schedule.state(index), Some(ChunkState::Done));
        }
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
            finish(&claims, &held);
        });
    }

    /// One fetcher reads a run of both chunks. The other has the end of
    /// that run put back while it is being read, and fetches what it can
    /// have. Under every interleaving, no chunk is held twice, the reader
    /// is told of the cut exactly when there was one, and afterwards
    /// nothing is left reserved.
    #[test]
    fn a_run_cut_while_it_is_read_is_never_held_twice() {
        loom::model(|| {
            let claims = Claims::new(&Bitfield::new(CHUNKS), || {});
            let held: Arc<Vec<AtomicBool>> =
                Arc::new((0..CHUNKS).map(|_| AtomicBool::new(false)).collect());
            let all = Bitfield::full(CHUNKS);
            let size = Size::Run {
                streams: 1,
                micros_each: 0,
            };
            let mut reserved = Claims::reserve(&claims, 0, &all, &mut 0, size, 0).unwrap();

            let reader = thread::spawn({
                let (claims, held) = (claims.clone(), held.clone());
                move || {
                    let mut read = 0;
                    loop {
                        match reserved.next() {
                            Next::Chunk(claim, _) => {
                                take_exclusively(&held, &claim);
                                reserved.learn(1, 0);
                                let_go(&held, &claim);
                                claims.complete(claim);
                                read += 1;
                            }
                            Next::Over => return (read, false),
                            Next::Cut(at) => {
                                assert_eq!(at, read);
                                return (read, true);
                            }
                        }
                    }
                }
            });
            let other = thread::spawn({
                let (claims, held) = (claims.clone(), held.clone());
                move || {
                    // Long after the run was begun: whatever is left of it
                    // is worth taking.
                    let spared = claims.spare(&Bitfield::full(CHUNKS), u64::MAX, 1);
                    let mut cursor = 0;
                    let mut fetched = 0;
                    while let Some(claim) = claim_exclusively(&claims, 1, &held, &mut cursor) {
                        let_go(&held, &claim);
                        claims.complete(claim);
                        fetched += 1;
                    }
                    (spared, fetched)
                }
            });
            let (read, cut) = reader.join().unwrap();
            let (spared, fetched) = other.join().unwrap();

            assert_eq!(cut, spared);
            assert_eq!(read + fetched, CHUNKS);
            finish(&claims, &held);
        });
    }

    /// One fetcher reads a chunk. Another takes that chunk over while it
    /// is being read, so that two are fetching it. Each then stores its
    /// copy or gives up, in either order. Under every interleaving the
    /// chunk is done exactly if whoever owned it at the end stored it,
    /// what the other did changes nothing, and afterwards nothing is left
    /// reserved.
    #[test]
    fn a_chunk_taken_over_while_it_is_read_is_never_stranded() {
        for (reader_stores, other_stores) in
            [(true, true), (true, false), (false, true), (false, false)]
        {
            loom::model(move || {
                let claims = Claims::new(&Bitfield::new(1), || {});
                let all = Bitfield::full(1);
                let mut reading = Claims::reserve(&claims, 0, &all, &mut 0, Size::One, 0).unwrap();

                let reader = thread::spawn({
                    let claims = claims.clone();
                    move || {
                        let Next::Chunk(claim, 0) = reading.next() else {
                            panic!("a run that was reserved has a chunk");
                        };
                        if reader_stores {
                            claims.complete(claim);
                        }
                    }
                });
                let other = thread::spawn({
                    let claims = claims.clone();
                    move || {
                        // Long after the chunk was asked for, by a fetcher
                        // that is quick: worth fetching a second time.
                        let all = Bitfield::full(1);
                        let Some(mut taken) = Claims::relieve(&claims, 1, &all, u64::MAX, 1) else {
                            return false;
                        };
                        let Next::Chunk(claim, 0) = taken.next() else {
                            panic!("a chunk that was taken over is there to take");
                        };
                        if other_stores {
                            claims.complete(claim);
                        }
                        true
                    }
                });
                reader.join().unwrap();
                let took_over = other.join().unwrap();

                // It is done exactly if its owner at the end stored it.
                let done = if took_over {
                    other_stores
                } else {
                    reader_stores
                };
                let state = claims.state();
                let expected = if done {
                    ChunkState::Done
                } else {
                    ChunkState::Missing
                };
                assert_eq!(state.schedule.state(0), Some(expected));
                assert_eq!(state.schedule.is_complete(), done);
                assert!(state.runs.is_empty());
            });
        }
    }
}
