//! How many chunks to ask one supplier for at the same time.
//!
//! A request takes a round trip before its first byte comes back. With too
//! few requests in flight, a fast route to a far supplier stands idle for
//! most of each round trip. With too many, a slow route shares itself out
//! between them and none finishes for a long time.
//!
//! So the number follows what is seen: how long a chunk took from being
//! asked for to being here. A chunk that came quickly shows there is room
//! for one more request. A chunk that came slowly shows there is one too
//! many. It starts at one, and each quick chunk adds one, so it doubles
//! with every round of answers until chunks stop being quick.
//!
//! This is for a supplier whose requests all travel on one connection, where
//! one more costs nothing but its bytes. Where each request is a connection
//! of its own, the number is set and does not change.

/// A chunk that came within this many milliseconds of being asked for was
/// quick.
pub const QUICK_MILLIS: u64 = 2_000;

/// A chunk that took more than this many milliseconds was slow.
pub const SLOW_MILLIS: u64 = 8_000;

/// The most bytes asked of one supplier at the same time.
pub const MOST_BYTES: u64 = 64 * 1024 * 1024;

/// The most requests in flight to one supplier, however small a chunk is.
pub const MOST_REQUESTS: u32 = 64;

/// The most requests to keep in flight for chunks of `chunk_bytes`: as many
/// as come to [`MOST_BYTES`], but at least one and at most
/// [`MOST_REQUESTS`].
pub fn most(chunk_bytes: u64) -> u64 {
    let fit = MOST_BYTES.checked_div(chunk_bytes).unwrap_or(u64::MAX);
    fit.clamp(1, u64::from(MOST_REQUESTS))
}

/// How many requests to keep in flight from now on, when there were to be
/// `requests` and a chunk has just taken `taken_millis` to come.
///
/// One more after a quick chunk, one fewer after a slow one, and never
/// fewer than one or more than `most`.
pub fn next(requests: u64, taken_millis: u64, most: u64) -> u64 {
    let wanted = if taken_millis < QUICK_MILLIS {
        requests.saturating_add(1)
    } else if taken_millis > SLOW_MILLIS {
        requests.saturating_sub(1)
    } else {
        requests
    };
    wanted.clamp(1, most.max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quick_chunks_add_a_request_and_slow_ones_take_one_away() {
        assert_eq!(next(4, QUICK_MILLIS - 1, 64), 5);
        assert_eq!(next(4, QUICK_MILLIS, 64), 4);
        assert_eq!(next(4, SLOW_MILLIS, 64), 4);
        assert_eq!(next(4, SLOW_MILLIS + 1, 64), 3);
    }

    #[test]
    fn there_is_always_one_request_and_never_more_than_the_most() {
        assert_eq!(next(1, u64::MAX, 64), 1);
        assert_eq!(next(64, 0, 64), 64);
        assert_eq!(next(0, SLOW_MILLIS + 1, 64), 1);
        // A number that is already past the most comes back down to it.
        assert_eq!(next(90, QUICK_MILLIS, 64), 64);
        assert_eq!(next(u64::MAX, 0, 64), 64);
        assert_eq!(next(3, 0, 0), 1);
    }

    #[test]
    fn the_most_requests_is_what_fits_in_the_most_bytes() {
        let mib = 1024 * 1024;
        assert_eq!(most(16 * mib), 4);
        assert_eq!(most(mib), 64);
        assert_eq!(most(mib + 1), 63);
        assert_eq!(most(4096), u64::from(MOST_REQUESTS));
        assert_eq!(most(MOST_BYTES), 1);
        assert_eq!(most(MOST_BYTES + 1), 1);
        assert_eq!(most(u64::MAX), 1);
        assert_eq!(most(0), u64::from(MOST_REQUESTS));
    }
}

/// Bounded proofs, run by `just kani`.
#[cfg(kani)]
mod proofs {
    use super::*;

    /// Whatever is seen, there is at least one request and never more than
    /// the most, the number moves by one at a time from inside those
    /// bounds, and it moves the right way.
    #[kani::proof]
    fn the_number_stays_in_bounds_and_moves_by_one() {
        let requests: u64 = kani::any();
        let taken: u64 = kani::any();
        let most: u64 = kani::any();
        let wanted = next(requests, taken, most);

        assert!(wanted >= 1);
        assert!(wanted <= most || most == 0);
        if most >= 1 && (1..=most).contains(&requests) {
            assert!(wanted.abs_diff(requests) <= 1);
            if taken < QUICK_MILLIS {
                assert!(wanted >= requests);
            } else if taken > SLOW_MILLIS {
                assert!(wanted <= requests);
            } else {
                assert!(wanted == requests);
            }
        }
    }

    /// For any size of chunk, the most requests is at least one and at
    /// most the limit, and what they ask for comes to no more than the
    /// most bytes unless a single chunk is already more.
    #[kani::proof]
    fn the_most_is_in_bounds_and_fits() {
        let chunk_bytes: u64 = kani::any();
        let most = most(chunk_bytes);

        assert!(most >= 1 && most <= u64::from(MOST_REQUESTS));
        if chunk_bytes <= MOST_BYTES {
            assert!(most * chunk_bytes <= MOST_BYTES);
        } else {
            assert!(most == 1);
        }
    }
}
