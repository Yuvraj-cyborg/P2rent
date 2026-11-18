//! A hash of a run of bytes that can be moved on by one byte.
//!
//! A destination that holds an older version of a file looks through it
//! for the chunks of the newer one, which may have moved. Hashing a chunk's
//! worth of bytes at every place in a file would cost the length of a chunk
//! at every byte. This hash costs two multiplications at every byte: the
//! hash of the bytes from one place is made from the hash of the bytes from
//! the place before.
//!
//! It is not a digest. Bytes can be made to share a hash, so what it finds
//! is only somewhere to look, and is kept only if its digest is right.
//!
//! This module is translated to Lean (see `proofs/`), so it is written
//! plainly: loops and indexing, no iterators or closures.

/// What the hash is multiplied by for each byte: an odd number, so that no
/// byte's part in the hash is ever multiplied away.
const FACTOR: u64 = 0x9e37_79b9_7f4a_7c15;

/// The hash of a window of bytes: a run of a set length, somewhere in a
/// file.
///
/// Each byte is multiplied by `FACTOR` once for every byte that follows it
/// in the window, and the products are added up, keeping the low 64 bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rolling {
    hash: u64,
    /// What the first byte of the window is multiplied by: `FACTOR` to the
    /// power of one less than the window's length.
    leaving: u64,
}

impl Rolling {
    /// The hash of `window`.
    pub fn of(window: &[u8]) -> Self {
        let mut hash: u64 = 0;
        let mut leaving: u64 = 1;
        let mut at = 0;
        while at < window.len() {
            hash = hash
                .wrapping_mul(FACTOR)
                .wrapping_add(u64::from(window[at]));
            if at > 0 {
                leaving = leaving.wrapping_mul(FACTOR);
            }
            at += 1;
        }
        Self { hash, leaving }
    }

    /// The hash, as a number to look up and to send.
    pub fn value(&self) -> u64 {
        self.hash
    }

    /// Moves the window on by one byte: `out`, its first byte, leaves it,
    /// and `inn` joins it after its last. The window keeps its length.
    ///
    /// The result is the hash that [`Rolling::of`] gives for the bytes of
    /// the new window.
    pub fn roll(&mut self, out: u8, inn: u8) {
        let without = self
            .hash
            .wrapping_sub(self.leaving.wrapping_mul(u64::from(out)));
        self.hash = without.wrapping_mul(FACTOR).wrapping_add(u64::from(inn));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    /// Bytes that do not repeat.
    fn bytes(len: usize) -> Vec<u8> {
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state.to_le_bytes()[0]
            })
            .collect()
    }

    #[test]
    fn rolling_through_a_file_gives_the_hash_of_each_window() {
        let file = bytes(300);
        for len in [1, 2, 7, 64, 255] {
            let mut rolling = Rolling::of(&file[..len]);
            for start in 1..=file.len() - len {
                rolling.roll(file[start - 1], file[start + len - 1]);
                assert_eq!(
                    rolling,
                    Rolling::of(&file[start..start + len]),
                    "{len} at {start}"
                );
            }
        }
    }

    #[test]
    fn the_hash_tells_windows_apart() {
        let file = bytes(64);
        let whole = Rolling::of(&file).value();
        // Another byte anywhere, and the same bytes in another order.
        for at in 0..file.len() {
            let mut other = file.clone();
            other[at] ^= 1;
            assert_ne!(Rolling::of(&other).value(), whole, "byte {at}");
        }
        let mut swapped = file.clone();
        swapped.swap(3, 40);
        assert_ne!(Rolling::of(&swapped).value(), whole);
        // And the same bytes hash the same, wherever they were read from.
        assert_eq!(Rolling::of(&file.clone()).value(), whole);
        assert_eq!(Rolling::of(&[]).value(), 0);
        assert_eq!(Rolling::of(&[7]).value(), 7);
    }
}
