//! How many chunks one stream asks for with one request.
//!
//! Where each request is a connection of its own, a stream asks for a run
//! of chunks in a row and reads them from one answer. The request ends
//! where the run ends, so nothing is sent that is not read, and the
//! connection is free for the next request when the answer is over.
//!
//! Between two requests a connection carries nothing for a round trip, so
//! a stream should make few. And a stream with nothing left to take stands
//! idle while the others finish, so the streams should end together. Both
//! are had by sharing out the work that is left by speed: a stream takes
//! as many chunks as it would fetch in the time the whole transfer still
//! needs. If every stream keeps its speed, each then makes one request
//! more and all end at the same moment. A stream that has fetched nothing
//! yet does not know its speed, and takes half of an even share of what
//! is free.
//!
//! Speeds change, and a stream may be left with more than it can fetch in
//! time. So when nothing is free, a stream with nothing to do takes over
//! the far end of what another has still to ask for: as much as it could
//! fetch by the time that other would get there. The answer that other is
//! reading then goes on past where it stops and has to be cut off, and
//! its connection with it, so this is done only where it brings the end
//! forward by more than [`WORTH_MICROS`]. A slow stream therefore takes
//! little from a fast one, or nothing.
//!
//! A connection is worth what its stream would fetch with it. So one that
//! is many times faster than the stream it takes from needs less of a
//! gain: as many times less. A web server far away that sends a megabyte
//! a second is then left with the chunks it is reading, and no more, when
//! a machine near by could have the rest in a moment.
//!
//! The chunk a stream is reading can be fetched by another as well, where
//! that brings it sooner by enough: see [`relieve`]. Whichever copy comes
//! first is kept. A transfer then does not end by waiting for the slowest
//! of its suppliers to finish what it happened to be reading.

/// What is worth cutting an answer off for: bringing the end of what it
/// carries forward by more than this.
pub const WORTH_MICROS: u64 = 2_000_000;

/// The length of time speeds are counted over: [`speed`] is in chunks for
/// each of these.
const SPEED_MICROS: u64 = 1_000_000_000;

/// How fast a stream is that takes `micros_each` microseconds for a chunk,
/// as a number that can be added to those of other streams. Zero if it is
/// not known (`micros_each` is zero), and never zero otherwise.
pub fn speed(micros_each: u64) -> u64 {
    match SPEED_MICROS.checked_div(micros_each) {
        Some(speed) => speed.max(1),
        None => 0,
    }
}

/// How long `work` chunks take streams whose speeds add up to `speeds`, in
/// microseconds. For ever, as far as a number can say it, if no speed is
/// known.
pub fn micros_left(work: u64, speeds: u64) -> u64 {
    work.saturating_mul(SPEED_MICROS)
        .checked_div(speeds)
        .unwrap_or(u64::MAX)
}

/// How many chunks a stream takes next, when `free` chunks are neither
/// held nor reserved, `streams` streams are taking runs, a chunk has
/// lately taken this stream `micros_each` (zero if none has come yet), and
/// the transfer is reckoned to need `micros_left` more.
///
/// At least one, and no more than are free. A stream by itself takes
/// everything. One that has fetched nothing takes half of an even share.
/// Any other takes what it would fetch in the time that is left, rounded
/// up: shares that come to a little more than the work leave the last
/// stream short, where shares that come to less would leave crumbs that
/// cost a request more.
pub fn length(free: u64, streams: u64, micros_each: u64, micros_left: u64) -> u64 {
    let most = free.max(1);
    if streams <= 1 {
        return most;
    }
    if micros_each == 0 {
        return (free / streams.saturating_mul(2)).clamp(1, most);
    }
    micros_left.div_ceil(micros_each).clamp(1, most)
}

/// How many of the `left` chunks a stream has still to ask for another
/// stream takes over, from the far end, when a chunk takes the first
/// `micros_each` and the other `mine` (zero if it has fetched nothing,
/// and it is then taken to be as fast as the first).
///
/// It is what the other can fetch by the time the first would get there:
/// the two then end together. Sooner than the first would have ended by
/// what the first takes for those chunks, and if that is not more than is
/// [`worth`] it, none are taken over. Nothing is known of a stream whose
/// `micros_each` is zero, and nothing is taken from it.
pub fn spare(left: u64, micros_each: u64, mine: u64) -> u64 {
    if micros_each == 0 {
        return 0;
    }
    let mine = if mine == 0 { micros_each } else { mine };
    // Shared out by speed, which is one over the time for a chunk, the
    // other's part of `left` is `micros_each / (micros_each + mine)` of
    // it. A part of a chunk counts as a chunk, so that a stream that has
    // stopped is relieved of the last chunk it holds too.
    let (wide_left, theirs, both) = (
        u128::from(left),
        u128::from(micros_each),
        u128::from(micros_each) + u128::from(mine),
    );
    let part = (wide_left * theirs).div_ceil(both);
    let part = u64::try_from(part).unwrap_or(left).min(left);
    if part.saturating_mul(micros_each) > worth(micros_each, mine) {
        part
    } else {
        0
    }
}

/// What a stream that takes `mine` for a chunk has to bring the end
/// forward by, in microseconds, to take over from one that takes
/// `micros_each`: [`WORTH_MICROS`] from a stream that is as fast or
/// faster, and as many times less from a slower one as that one is slower.
///
/// Taking over costs the other stream its connection, and a connection is
/// worth what would be fetched with it. What a stream ten times slower
/// would fetch in the two seconds a new connection may cost it is what
/// this one fetches in a fifth of a second.
pub fn worth(micros_each: u64, mine: u64) -> u64 {
    if mine >= micros_each {
        return WORTH_MICROS;
    }
    let scaled = u128::from(WORTH_MICROS) * u128::from(mine) / u128::from(micros_each.max(1));
    // Less than the whole, since `mine` is the smaller: and said here, so
    // that it holds whatever the division gives.
    u64::try_from(scaled)
        .unwrap_or(WORTH_MICROS)
        .min(WORTH_MICROS)
}

/// Whether a stream with nothing to do should fetch the chunk another is
/// reading as well: that other takes `micros_each` for a chunk and has
/// been reading this one for `waited`, and a chunk takes this stream
/// `mine`. Never if either time is not known (zero).
///
/// The other is reckoned to need the rest of its `micros_each`, or, if it
/// is past that, as long again as it has taken. This stream would have the
/// chunk sooner by that less `mine`, and fetches it where that is more
/// than is [`worth`] it. What the other was sent of the chunk is then for
/// nothing, so a stream that is no faster does this only for a chunk that
/// is long overdue.
pub fn relieve(micros_each: u64, waited: u64, mine: u64) -> bool {
    if micros_each == 0 || mine == 0 {
        return false;
    }
    let left = if waited < micros_each {
        micros_each - waited
    } else {
        waited
    };
    left.saturating_sub(mine) > worth(micros_each, mine)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A millisecond.
    const MS: u64 = 1_000;

    #[test]
    fn a_chunk_that_is_being_read_is_fetched_by_a_much_faster_stream_too() {
        // A chunk takes the reader eight seconds, and it has been at this
        // one for one. A stream that takes 10 ms would have it seven
        // seconds sooner.
        assert!(relieve(8_000 * MS, 1_000 * MS, 10 * MS));
        // Nearly done, there is nothing to gain: 5 ms are left.
        assert!(!relieve(8_000 * MS, 7_995 * MS, 10 * MS));
        // A stream that is as fast leaves it: the reader has a head start.
        assert!(!relieve(8_000 * MS, 1_000 * MS, 8_000 * MS));
        assert!(!relieve(250 * MS, 0, 250 * MS));
        // One that is slower, all the more.
        assert!(!relieve(250 * MS, 100 * MS, 1_000 * MS));
    }

    #[test]
    fn a_chunk_that_is_long_overdue_is_fetched_by_a_stream_of_any_speed() {
        // It should have taken 250 ms and has taken three seconds. It is
        // reckoned to need three more, which is over two more than a
        // stream of the same speed would take.
        assert!(relieve(250 * MS, 3_000 * MS, 250 * MS));
        // At two seconds it is not yet worth a second request.
        assert!(!relieve(250 * MS, 2_000 * MS, 250 * MS));
        // Nothing is done where a speed is not known.
        assert!(!relieve(0, u64::MAX, 250 * MS));
        assert!(!relieve(250 * MS, u64::MAX, 0));
        assert!(relieve(1, u64::MAX, 1));
    }

    #[test]
    fn a_first_run_is_half_an_even_share_of_what_is_free() {
        // Four streams and 128 chunks free: a sixteenth of them each.
        assert_eq!(length(128, 4, 0, 0), 16);
        assert_eq!(length(64, 4, 0, u64::MAX), 8);
        assert_eq!(length(9, 4, 0, 0), 1);
        assert_eq!(length(7, 2, 0, 0), 1);
        assert_eq!(length(8, 2, 0, 0), 2);
    }

    #[test]
    fn a_later_run_is_what_the_stream_fetches_in_the_time_left() {
        // A chunk takes 250 ms and the transfer needs five seconds more.
        assert_eq!(length(128, 4, 250 * MS, 5_000 * MS), 20);
        // Part of a chunk counts as a chunk.
        assert_eq!(length(128, 4, 250 * MS, 5_000 * MS + 1), 21);
        assert_eq!(length(128, 4, 250 * MS, 5_000 * MS - 1), 20);
        assert_eq!(length(128, 4, 250 * MS, 1), 1);
        // No more than are free.
        assert_eq!(length(12, 4, 250 * MS, 5_000 * MS), 12);
        // With no time left there is still a chunk to take.
        assert_eq!(length(128, 4, 250 * MS, 0), 1);
        assert_eq!(length(5, 4, u64::MAX, u64::MAX), 1);
        assert_eq!(length(5, 4, u64::MAX, 0), 1);
        assert_eq!(length(5, 4, 1, u64::MAX), 5);
    }

    #[test]
    fn a_stream_by_itself_takes_everything() {
        assert_eq!(length(128, 1, 0, 0), 128);
        assert_eq!(length(128, 1, 250 * MS, 1_000 * MS), 128);
        assert_eq!(length(128, 0, 250 * MS, 1_000 * MS), 128);
    }

    #[test]
    fn a_run_is_at_least_one_chunk() {
        assert_eq!(length(0, 4, 0, 0), 1);
        assert_eq!(length(0, 1, 0, 0), 1);
        assert_eq!(length(0, 4, 250 * MS, 5_000 * MS), 1);
        assert_eq!(length(1, u64::MAX, 0, 0), 1);
        assert_eq!(length(u64::MAX, u64::MAX, 1, 0), 1);
        assert_eq!(length(u64::MAX, 2, 1, u64::MAX), u64::MAX);
    }

    #[test]
    fn speeds_add_up_to_the_time_that_is_left() {
        // Four streams at 250 ms a chunk fetch sixteen chunks a second.
        let speeds = 4 * speed(250 * MS);
        assert_eq!(micros_left(80, speeds), 5_000 * MS);
        assert_eq!(micros_left(0, speeds), 0);
        // A stream three times slower counts for a third.
        assert_eq!(speed(250 * MS), 3 * speed(750 * MS) + 1);
        // A chunk that takes a microsecond can still be told from one
        // that takes two.
        assert_eq!(speed(1), 2 * speed(2));
        // A speed that is known is never nothing, however slow.
        assert_eq!(speed(SPEED_MICROS), 1);
        assert_eq!(speed(u64::MAX), 1);
        // One that is not known is nothing, and then no time can be told.
        assert_eq!(speed(0), 0);
        assert_eq!(micros_left(80, 0), u64::MAX);
        assert_eq!(micros_left(u64::MAX, 1), u64::MAX);
    }

    #[test]
    fn streams_of_one_speed_share_what_is_left_when_that_saves_time() {
        // At 250 ms a chunk, taking over half of sixteen brings the end
        // forward by two seconds, which is not yet worth it. Half of
        // seventeen, rounded up, is.
        assert_eq!(spare(16, 250 * MS, 250 * MS), 0);
        assert_eq!(spare(17, 250 * MS, 250 * MS), 9);
        assert_eq!(spare(40, 250 * MS, 250 * MS), 20);
        // A stream that has fetched nothing is taken to be as fast.
        assert_eq!(spare(40, 250 * MS, 0), 20);
        // One chunk that takes long is given up whole.
        assert_eq!(spare(1, WORTH_MICROS, WORTH_MICROS), 0);
        assert_eq!(spare(1, WORTH_MICROS + 1, WORTH_MICROS + 1), 1);
        assert_eq!(spare(0, u64::MAX, 1), 0);
    }

    #[test]
    fn a_slow_stream_takes_little_from_a_fast_one_or_nothing() {
        // Forty chunks left to a stream that takes 125 ms for each: five
        // seconds. One that is eight times slower could fetch five of
        // them in that time, which saves under a second.
        assert_eq!(spare(40, 125 * MS, 1_000 * MS), 0);
        // With a minute's worth left to the fast one, a ninth of it is
        // worth taking.
        assert_eq!(spare(480, 125 * MS, 1_000 * MS), 54);
    }

    #[test]
    fn a_faster_stream_needs_less_of_a_gain() {
        assert_eq!(worth(250 * MS, 250 * MS), WORTH_MICROS);
        // A slower one needs the whole of it, however slow.
        assert_eq!(worth(250 * MS, 1_000 * MS), WORTH_MICROS);
        assert_eq!(worth(1, u64::MAX), WORTH_MICROS);
        // One ten times faster needs a tenth.
        assert_eq!(worth(250 * MS, 25 * MS), WORTH_MICROS / 10);
        assert_eq!(worth(u64::MAX, 1), 0);

        // Three chunks left to a stream that takes 100 ms for each. One as
        // fast leaves them: under a third of a second is to be had. One a
        // hundred times faster takes them all.
        assert_eq!(spare(3, 100 * MS, 100 * MS), 0);
        assert_eq!(spare(3, 100 * MS, MS), 3);
        // The last chunk too: it is not being read yet.
        assert_eq!(spare(1, 100 * MS, MS), 1);
    }

    #[test]
    fn a_fast_stream_takes_most_from_a_slow_one() {
        // Nine chunks left at a second each: one that is eight times
        // faster takes eight of them.
        assert_eq!(spare(9, 1_000 * MS, 125 * MS), 8);
        // From a stream that has all but stopped, everything.
        assert_eq!(spare(9, 3_600_000 * MS, 125 * MS), 9);
        assert_eq!(spare(1, 3_000 * MS, 125 * MS), 1);
        assert_eq!(spare(u64::MAX, u64::MAX, 1), u64::MAX);
        assert_eq!(spare(u64::MAX, u64::MAX, u64::MAX), u64::MAX / 2 + 1);
    }

    #[test]
    fn nothing_is_taken_from_a_stream_of_which_nothing_is_known() {
        assert_eq!(spare(u64::MAX, 0, 0), 0);
        assert_eq!(spare(u64::MAX, 0, 250 * MS), 0);
    }
}

/// Bounded proofs, run by `just kani`.
#[cfg(kani)]
mod proofs {
    use super::*;

    /// A run is at least one chunk and no more than are free, and a stream
    /// by itself takes all that is free.
    #[kani::proof]
    fn a_run_is_within_what_is_free() {
        let free: u64 = kani::any();
        let streams: u64 = kani::any();
        let micros_each: u64 = kani::any();
        let micros_left: u64 = kani::any();
        let run = length(free, streams, micros_each, micros_left);

        assert!(run >= 1);
        assert!(run <= free || free == 0);
        if streams <= 1 && free > 0 {
            assert!(run == free);
        }
    }

    /// A speed is nothing exactly when it is not known, and no arithmetic
    /// on speeds can fail.
    #[kani::proof]
    fn a_known_speed_is_never_nothing() {
        let micros_each: u64 = kani::any();
        assert!((speed(micros_each) == 0) == (micros_each == 0));

        let work: u64 = kani::any();
        let speeds: u64 = kani::any();
        let left = micros_left(work, speeds);
        if work == 0 && speeds > 0 {
            assert!(left == 0);
        }
    }

    /// What is taken over is never more than is left, is something only
    /// when it brings the end forward by more than is worth it, and never
    /// leaves the slower of two streams with the larger part.
    #[kani::proof]
    fn what_is_spared_is_within_what_is_left_and_worth_it() {
        let left: u64 = kani::any();
        let micros_each: u64 = kani::any();
        let mine: u64 = kani::any();
        let spared = spare(left, micros_each, mine);

        assert!(spared <= left);
        if spared > 0 {
            assert!(micros_each > 0);
            // A stream that is no faster has to gain the whole of what a
            // connection is worth.
            if mine == 0 || mine >= micros_each {
                assert!(spared.saturating_mul(micros_each) > WORTH_MICROS);
            }
        }
        if micros_each == 0 {
            assert!(spared == 0);
        }
    }

    /// A chunk that is being read is fetched a second time only by a
    /// stream that would have it sooner, and never where a speed is not
    /// known. A stream that is no faster does it only for a chunk that has
    /// taken longer than a connection is worth beyond its own time.
    #[kani::proof]
    fn a_chunk_is_fetched_twice_only_to_have_it_sooner() {
        let micros_each: u64 = kani::any();
        let waited: u64 = kani::any();
        let mine: u64 = kani::any();
        if relieve(micros_each, waited, mine) {
            assert!(micros_each > 0 && mine > 0);
            let left = if waited < micros_each {
                micros_each - waited
            } else {
                waited
            };
            assert!(mine < left);
            if mine >= micros_each {
                assert!(waited > WORTH_MICROS);
            }
        }
    }

    /// What a take-over has to gain is never more than a connection is
    /// worth, and is all of that for a stream that is no faster.
    ///
    /// That a faster stream never has to gain more than a slower one is
    /// not proven here: the division in it is beyond the solver, even for
    /// 16-bit times. A property test in `tests/properties.rs` tries it
    /// for times of every size.
    #[kani::proof]
    fn a_take_over_never_has_to_gain_more_than_a_connection_is_worth() {
        let micros_each: u64 = kani::any();
        let mine: u64 = kani::any();

        assert!(worth(micros_each, mine) <= WORTH_MICROS);
        if mine >= micros_each {
            assert!(worth(micros_each, mine) == WORTH_MICROS);
        }
    }
}
