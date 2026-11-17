//! When to try again.
//!
//! One rule for every supplier, whatever it is, so that no backend invents
//! its own.

/// The longest wait between two attempts, in seconds.
pub const LONGEST_WAIT: u64 = 30;

/// How long to wait, in seconds, before trying a supplier again after it
/// has failed `failures` times in a row: twice as long each time, up to
/// [`LONGEST_WAIT`].
pub fn wait_seconds(failures: u32) -> u64 {
    // Thirty-two seconds is already past the longest wait, so the doubling
    // need go no further, and the shift cannot overflow.
    let doubled = 1u64 << if failures < 5 { failures } else { 5 };
    if doubled < LONGEST_WAIT {
        doubled
    } else {
        LONGEST_WAIT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wait_doubles_up_to_a_limit() {
        let waits: Vec<u64> = (0..8).map(wait_seconds).collect();
        assert_eq!(waits, [1, 2, 4, 8, 16, 30, 30, 30]);
        assert_eq!(wait_seconds(u32::MAX), LONGEST_WAIT);
    }
}

/// Bounded proof, run by `just kani`: for every count of failures, the wait
/// is at least a second, never past the limit, and never shorter than the
/// wait before it.
#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    fn the_wait_never_shrinks_and_stays_in_bounds() {
        let failures: u32 = kani::any();
        let wait = wait_seconds(failures);
        assert!(1 <= wait && wait <= LONGEST_WAIT);
        if failures > 0 {
            assert!(wait_seconds(failures - 1) <= wait);
        }
    }
}
