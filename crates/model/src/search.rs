//! Looking through bytes for chunks that are wanted, wherever they lie.
//!
//! A destination that holds an older version of a file has most of the
//! newer one already, but not always in the same place: a part put in near
//! the start moves everything after it. So before a chunk is fetched, what
//! the destination holds is gone through once for it.
//!
//! Hashing a chunk's worth of bytes at every place would cost the length
//! of a chunk at every byte. The [rolling hash](crate::rolling) costs two
//! multiplications at every byte, and says where to look: only where it
//! is the hash of a wanted chunk is the digest of the bytes worked out.
//! The digest decides. The rolling hash of a chunk is a hint that its
//! source gives, and a wrong hint costs time and can put no wrong byte in
//! a file.
//!
//! Bytes can be made to share a rolling hash. A file made of them would
//! have a digest worked out at every byte, so a search gives up after a
//! set number of places where the hash was right and the digest was not.

use crate::artifact::Hash;
use crate::rolling::Rolling;
use alloc::collections::BTreeMap;
use alloc::vec;
use alloc::vec::Vec;

/// How many bits the quick filter has: one for each value of the top
/// twenty bits of a rolling hash.
const FILTER_BITS: usize = 1 << 20;

/// The chunks of one length that a destination lacks and looks for.
#[derive(Debug, Clone)]
pub struct Wanted {
    /// The length of each, in bytes.
    len: usize,
    /// The chunks that are still looked for, by rolling hash. Chunks that
    /// hold the same bytes share a hash and a digest, and are all found at
    /// once.
    by_hash: BTreeMap<u64, Vec<(u64, Hash)>>,
    /// Nearly every place in a file is the start of no wanted chunk. A bit
    /// for each of a million values of the hash says so at once, and the
    /// table is asked only where a bit is set.
    maybe: Vec<u64>,
    /// How many chunks are still looked for.
    left: usize,
    /// How many more places may have the right rolling hash and the wrong
    /// digest before the search gives up.
    misses_left: u64,
}

/// How a search of one block of bytes ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Searched {
    /// The block was gone through. The next block is to begin with the
    /// bytes of this one from this place on: a chunk may lie across the
    /// two.
    On(usize),
    /// Nothing is left to look for: every chunk was found.
    Done,
    /// Too many places had the hash of a chunk and not its bytes. What was
    /// found stands, and nothing more is looked for.
    GaveUp,
}

impl Wanted {
    /// Nothing wanted yet, of chunks `len` bytes long. The search gives up
    /// after `misses` places with the right rolling hash and the wrong
    /// digest.
    pub fn new(len: u32, misses: u64) -> Self {
        Self {
            len: len as usize,
            by_hash: BTreeMap::new(),
            maybe: vec![0; FILTER_BITS / 64],
            left: 0,
            misses_left: misses,
        }
    }

    /// Looks for chunk `index` as well: bytes whose rolling hash is
    /// `rolling` and whose digest is `digest`.
    pub fn add(&mut self, index: u64, rolling: u64, digest: Hash) {
        self.by_hash
            .entry(rolling)
            .or_default()
            .push((index, digest));
        let bit = filter_bit(rolling);
        self.maybe[bit / 64] |= 1 << (bit % 64);
        self.left += 1;
    }

    /// How many chunks are still looked for.
    pub fn left(&self) -> usize {
        self.left
    }

    /// Goes through `block` for the chunks that are wanted, and calls
    /// `found` with the index of each one found and where in the block its
    /// bytes begin. A chunk that was found is not looked for again.
    ///
    /// A file is searched block by block: see [`Searched::On`].
    pub fn search(&mut self, block: &[u8], found: &mut dyn FnMut(u64, usize)) -> Searched {
        if self.left == 0 {
            return Searched::Done;
        }
        if self.misses_left == 0 {
            return Searched::GaveUp;
        }
        let len = self.len;
        if len == 0 || block.len() < len {
            return Searched::On(0);
        }
        let mut hash = Rolling::of(&block[..len]);
        let mut at = 0;
        loop {
            let value = hash.value();
            let bit = filter_bit(value);
            if self.maybe[bit / 64] & (1 << (bit % 64)) != 0 && self.by_hash.contains_key(&value) {
                let here = Hash::of(&block[at..at + len]);
                if self.take(value, here, at, found) {
                    if self.left == 0 {
                        return Searched::Done;
                    }
                } else {
                    self.misses_left -= 1;
                    if self.misses_left == 0 {
                        return Searched::GaveUp;
                    }
                }
            }
            if at + len >= block.len() {
                // The last window was looked at. The next begins a byte on.
                return Searched::On(at + 1);
            }
            hash.roll(block[at], block[at + len]);
            at += 1;
        }
    }

    /// Reports the wanted chunks with rolling hash `value` whose digest is
    /// `here`, as found at `at`, and stops looking for them. Returns
    /// whether there were any.
    fn take(
        &mut self,
        value: u64,
        here: Hash,
        at: usize,
        found: &mut dyn FnMut(u64, usize),
    ) -> bool {
        let Some(candidates) = self.by_hash.get_mut(&value) else {
            return false;
        };
        let before = candidates.len();
        candidates.retain(|&(index, digest)| {
            let is_it = digest == here;
            if is_it {
                found(index, at);
            }
            !is_it
        });
        let taken = before - candidates.len();
        if candidates.is_empty() {
            self.by_hash.remove(&value);
        }
        self.left -= taken;
        taken > 0
    }
}

/// Which bit of the filter stands for a rolling hash. The top bits are
/// used: they have been mixed by the most multiplications.
fn filter_bit(rolling: u64) -> usize {
    // Twenty bits, so the number fits whatever the size of a pointer.
    (rolling >> 44) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The length of a chunk in these tests, as a count and as a size.
    const CHUNK: u32 = 64;
    const LEN: usize = CHUNK as usize;

    /// Bytes that do not repeat, and differ for each `seed`.
    fn bytes(len: usize, seed: u64) -> Vec<u8> {
        let mut state = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state.to_le_bytes()[0]
            })
            .collect()
    }

    /// Every whole chunk of `new` as something to look for.
    fn wanted(new: &[u8], misses: u64) -> Wanted {
        let mut wanted = Wanted::new(CHUNK, misses);
        for (index, chunk) in (0u64..).zip(new.as_chunks::<LEN>().0) {
            wanted.add(index, Rolling::of(chunk).value(), Hash::of(chunk));
        }
        wanted
    }

    /// Searches `old` in blocks of `block` bytes, as a file is searched,
    /// and gives what was found, in the order it was found.
    fn search(wanted: &mut Wanted, old: &[u8], block: usize) -> (Vec<(u64, usize)>, Searched) {
        let mut finds = Vec::new();
        let (mut from, mut end) = (0, block.min(old.len()));
        loop {
            let ended = wanted.search(&old[from..end], &mut |index, at| {
                finds.push((index, from + at));
            });
            match ended {
                Searched::On(kept) if end < old.len() => {
                    from += kept;
                    end = (end + block).min(old.len());
                }
                ended => return (finds, ended),
            }
        }
    }

    #[test]
    fn a_chunk_is_found_wherever_it_lies() {
        let new = bytes(LEN * 4, 1);
        // The old file has seven bytes more at its start, so that every
        // chunk of the new one lies seven bytes further on in it.
        let mut old = bytes(7, 2);
        old.extend_from_slice(&new);

        let mut looking = wanted(&new, 100);
        let (finds, ended) = search(&mut looking, &old, old.len());
        assert_eq!(
            finds,
            [(0, 7), (1, 7 + LEN), (2, 7 + 2 * LEN), (3, 7 + 3 * LEN)]
        );
        assert_eq!(ended, Searched::Done);
        assert_eq!(looking.left(), 0);
    }

    #[test]
    fn a_chunk_that_is_not_there_is_not_found() {
        let new = bytes(LEN * 3, 3);
        // The middle chunk has one byte changed in the old file.
        let mut old = new.clone();
        old[LEN + 5] ^= 1;

        let mut looking = wanted(&new, 100);
        let (finds, ended) = search(&mut looking, &old, old.len());
        assert_eq!(finds, [(0, 0), (2, 2 * LEN)]);
        assert_eq!(ended, Searched::On(old.len() - LEN + 1));
        assert_eq!(looking.left(), 1);
    }

    #[test]
    fn a_file_is_searched_block_by_block_and_nothing_between_two_is_missed() {
        let new = bytes(LEN * 6, 4);
        let mut old = bytes(11, 5);
        old.extend_from_slice(&new);
        let whole = search(&mut wanted(&new, 100), &old, old.len()).0;
        assert_eq!(whole.len(), 6);
        // Blocks of every size from a chunk up: a chunk lies across two
        // of them again and again.
        for block in LEN..LEN * 3 {
            let (finds, ended) = search(&mut wanted(&new, 100), &old, block);
            assert_eq!(finds, whole, "blocks of {block}");
            assert_eq!(ended, Searched::Done, "blocks of {block}");
        }
    }

    #[test]
    fn chunks_that_hold_the_same_bytes_are_found_together() {
        let chunk = bytes(LEN, 6);
        let mut new = chunk.clone();
        new.extend_from_slice(&bytes(LEN, 7));
        new.extend_from_slice(&chunk);
        let mut old = bytes(3, 8);
        old.extend_from_slice(&chunk);

        let mut looking = wanted(&new, 100);
        let (finds, _) = search(&mut looking, &old, old.len());
        assert_eq!(finds, [(0, 3), (2, 3)]);
        assert_eq!(looking.left(), 1);
    }

    #[test]
    fn a_hint_that_is_wrong_finds_nothing_and_a_search_gives_up_on_too_many() {
        let old = bytes(LEN * 8, 9);
        // Asked to look for bytes that have the rolling hash of what is
        // at each place of the old file, and another digest.
        let mut looking = Wanted::new(CHUNK, 5);
        for at in 0..20 {
            let hash = Rolling::of(&old[at..at + LEN]).value();
            looking.add(at as u64, hash, Hash::of(b"something else"));
        }
        let (finds, ended) = search(&mut looking, &old, old.len());
        assert_eq!(finds, []);
        assert_eq!(ended, Searched::GaveUp);
        // And it stays given up.
        let again = looking.search(&old, &mut |_, _| panic!("nothing is found"));
        assert_eq!(again, Searched::GaveUp);
    }

    #[test]
    fn what_is_too_short_to_hold_a_chunk_holds_none() {
        let new = bytes(LEN * 2, 10);
        let mut looking = wanted(&new, 100);
        let none = &mut |_, _| panic!("nothing is found");
        assert_eq!(looking.search(&new[..LEN - 1], none), Searched::On(0));
        assert_eq!(looking.search(&[], none), Searched::On(0));
        // With nothing wanted there is nothing to do.
        let mut nothing = Wanted::new(CHUNK, 100);
        assert_eq!(nothing.search(&new, none), Searched::Done);
        // Chunks of no length are not looked for.
        let mut empty = Wanted::new(0, 100);
        empty.add(0, 0, Hash::of(b""));
        assert_eq!(empty.search(&new, none), Searched::On(0));
    }
}
