//! Which chunks of an artifact are in place, and which of its files are
//! whole.
//!
//! A download writes chunks into part files, and gives a file its final name
//! only when every one of its chunks is in place. This module holds that
//! decision and nothing else. It is translated to Lean and proven (see
//! `proofs/`), so it is written plainly: loops and indexing, no iterators or
//! closures.

use crate::artifact::{Bitfield, Layout};
use alloc::vec::Vec;

/// The chunks that are in place, and how many each file still lacks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Assembly {
    layout: Layout,
    held: Bitfield,
    /// For each file, the number of its chunks that are not in place.
    missing: Vec<u64>,
}

impl Assembly {
    /// Starts from the chunks already in place. A set whose length is not
    /// the layout's number of chunks cannot describe it, and counts as
    /// empty.
    pub fn new(layout: Layout, held: Bitfield) -> Self {
        let held = if held.len() == layout.chunk_count() {
            held
        } else {
            Bitfield::new(layout.chunk_count())
        };
        let mut missing = Vec::with_capacity(layout.file_count());
        let mut file = 0;
        while file < layout.file_count() {
            let (start, end) = layout.file_chunks(file);
            let mut lacking: u64 = 0;
            let mut index = start;
            while index < end {
                if !held.get(index) {
                    lacking += 1;
                }
                index += 1;
            }
            missing.push(lacking);
            file += 1;
        }
        Self {
            layout,
            held,
            missing,
        }
    }

    /// The chunks that are in place.
    pub fn held(&self) -> &Bitfield {
        &self.held
    }

    /// Whether chunk `index` is in place. `false` for an index out of range.
    pub fn has(&self, index: u64) -> bool {
        self.held.get(index)
    }

    /// Whether every chunk of every file is in place.
    pub fn is_complete(&self) -> bool {
        self.held.is_full()
    }

    /// Whether every chunk of `file` is in place. A file with no chunks is
    /// whole from the start. `false` if there is no such file.
    pub fn is_whole(&self, file: usize) -> bool {
        file < self.missing.len() && self.missing[file] == 0
    }

    /// Records that chunk `index` is in place. Returns the file it belongs
    /// to if that was the last chunk the file lacked: this is the one moment
    /// at which the file may be given its final name.
    ///
    /// A chunk that is already in place, or an index out of range, changes
    /// nothing and returns `None`.
    pub fn add(&mut self, index: u64) -> Option<usize> {
        if self.held.get(index) {
            return None;
        }
        match self.layout.locate(index) {
            None => None,
            Some(chunk) => {
                self.held.set(index);
                let left = self.missing[chunk.file] - 1;
                self.missing[chunk.file] = left;
                if left == 0 { Some(chunk.file) } else { None }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Files of 25, 0 and 10 bytes in chunks of 10: chunks 0 to 2 belong to
    /// the first file, none to the second, and chunk 3 to the third.
    fn layout() -> Layout {
        Layout::new(&[25, 0, 10], 10).unwrap()
    }

    #[test]
    fn starts_from_what_is_held() {
        let mut held = Bitfield::new(4);
        held.set(1);
        held.set(3);
        let assembly = Assembly::new(layout(), held.clone());

        assert_eq!(assembly.held(), &held);
        assert!(assembly.has(1) && !assembly.has(0) && !assembly.has(4));
        assert!(!assembly.is_whole(0));
        assert!(assembly.is_whole(1), "a file with no chunks is whole");
        assert!(assembly.is_whole(2));
        assert!(!assembly.is_whole(3), "there is no fourth file");
        assert!(!assembly.is_complete());
    }

    #[test]
    fn a_file_becomes_whole_with_its_last_chunk_and_only_then() {
        let mut assembly = Assembly::new(layout(), Bitfield::new(4));
        assert_eq!(assembly.add(2), None);
        assert_eq!(assembly.add(0), None);
        assert!(!assembly.is_whole(0));
        assert_eq!(assembly.add(1), Some(0));
        assert!(assembly.is_whole(0));
        assert!(!assembly.is_complete());
        assert_eq!(assembly.add(3), Some(2));
        assert!(assembly.is_complete());
    }

    #[test]
    fn adding_a_chunk_twice_or_out_of_range_changes_nothing() {
        let mut assembly = Assembly::new(layout(), Bitfield::new(4));
        assert_eq!(assembly.add(3), Some(2));
        let before = assembly.clone();
        assert_eq!(assembly.add(3), None, "the file is announced once");
        assert_eq!(assembly.add(4), None);
        assert_eq!(assembly.add(u64::MAX), None);
        assert_eq!(assembly, before);
    }

    #[test]
    fn a_set_of_the_wrong_length_counts_as_empty() {
        let assembly = Assembly::new(layout(), Bitfield::full(3));
        assert_eq!(assembly.held(), &Bitfield::new(4));
        assert!(!assembly.is_whole(0) && !assembly.is_whole(2));
    }

    #[test]
    fn no_files_is_complete() {
        let assembly = Assembly::new(Layout::new(&[], 10).unwrap(), Bitfield::new(0));
        assert!(assembly.is_complete());
        assert!(!assembly.is_whole(0));
    }
}

/// Bounded proofs, run by `just kani`. They hold for `FILES` files of every
/// number of chunks up to `MAX_FILE_CHUNKS` each, for every set of chunks
/// already in place and every chunk added. An assembly does not look at
/// sizes in bytes, so the chunk size is fixed.
#[cfg(kani)]
mod proofs {
    use super::*;

    const FILES: usize = 2;
    const MAX_FILE_CHUNKS: u64 = 2;
    const CHUNK_SIZE: u32 = 4;

    /// Files with the given numbers of whole chunks.
    fn layout_of(chunks: [u64; FILES]) -> Layout {
        let mut sizes = [0; FILES];
        let mut file = 0;
        while file < FILES {
            sizes[file] = chunks[file] * u64::from(CHUNK_SIZE);
            file += 1;
        }
        let Some(layout) = Layout::new(&sizes, CHUNK_SIZE) else {
            unreachable!("sizes this small cannot overflow");
        };
        layout
    }

    fn any_held(layout: &Layout) -> Bitfield {
        let mut held = Bitfield::new(layout.chunk_count());
        let mut index = 0;
        while index < layout.chunk_count() {
            if kani::any() {
                held.set(index);
            }
            index += 1;
        }
        held
    }

    /// How many chunks of `file` are not in place, counted the slow way.
    fn lacking(assembly: &Assembly, file: usize) -> u64 {
        let (start, end) = assembly.layout.file_chunks(file);
        let mut count = 0;
        let mut index = start;
        while index < end {
            if !assembly.held.get(index) {
                count += 1;
            }
            index += 1;
        }
        count
    }

    fn counts_are_right(assembly: &Assembly) {
        let mut file = 0;
        while file < FILES {
            assert!(assembly.missing[file] == lacking(assembly, file));
            assert!(assembly.is_whole(file) == (lacking(assembly, file) == 0));
            file += 1;
        }
    }

    /// Adding a chunk announces a file only when that file has just become
    /// whole, changes no other chunk, and keeps every count right.
    fn check_add(chunks: [u64; FILES]) {
        let layout = layout_of(chunks);
        let held = any_held(&layout);
        let mut assembly = Assembly::new(layout, held);
        counts_are_right(&assembly);
        let before = assembly.clone();
        let index: u64 = kani::any();
        let other: u64 = kani::any();

        let announced = assembly.add(index);

        counts_are_right(&assembly);
        if other != index {
            assert!(assembly.has(other) == before.has(other));
        }
        let in_range = index < assembly.layout.chunk_count();
        assert!(assembly.has(index) == (in_range || before.has(index)));
        match announced {
            Some(file) => {
                assert!(in_range && !before.has(index));
                assert!(assembly.is_whole(file) && !before.is_whole(file));
            }
            None => {
                let mut file = 0;
                while file < FILES {
                    assert!(assembly.is_whole(file) == before.is_whole(file));
                    file += 1;
                }
            }
        }
    }

    #[kani::proof]
    #[kani::unwind(6)]
    fn a_file_is_announced_exactly_when_it_becomes_whole() {
        let mut first = 0;
        while first <= MAX_FILE_CHUNKS {
            let mut second = 0;
            while second <= MAX_FILE_CHUNKS {
                check_add([first, second]);
                second += 1;
            }
            first += 1;
        }
    }
}
