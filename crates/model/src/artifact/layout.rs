//! Where each chunk lives: pure arithmetic over file sizes.
//!
//! This module is translated to Lean and proven (see `proofs/`), so it is
//! written plainly: loops and indexing, no iterators or closures, and no
//! strings or hashes.

use alloc::vec::Vec;

/// Where a chunk lives inside an artifact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChunkLocation {
    /// Index of the file the chunk belongs to.
    pub file: usize,
    /// Byte offset of the chunk within that file.
    pub offset: u64,
    /// Length of the chunk in bytes.
    pub len: u32,
}

/// How a list of files is cut into chunks.
///
/// Each file is chunked on its own, so a file's chunk `n` starts at byte
/// `n * chunk_size`. Chunks are numbered across all files, in file order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    chunk_size: u32,
    /// Size of each file in bytes.
    sizes: Vec<u64>,
    /// Number of the first chunk of each file.
    first_chunk: Vec<u64>,
    chunk_count: u64,
    total_size: u64,
}

/// Chunks needed for a file of `size` bytes. `chunk_size` must not be zero.
fn chunks_in(size: u64, chunk_size: u64) -> u64 {
    let whole = size / chunk_size;
    let rest = size % chunk_size;
    if rest == 0 { whole } else { whole + 1 }
}

impl Layout {
    /// Lays out files of the given sizes. Returns `None` if the chunk size
    /// is zero or the sizes add up to more than a `u64` can hold.
    pub fn new(sizes: &[u64], chunk_size: u32) -> Option<Self> {
        if chunk_size == 0 {
            return None;
        }
        let mut first_chunk = Vec::with_capacity(sizes.len());
        let mut chunk_count: u64 = 0;
        let mut total_size: u64 = 0;
        let mut file = 0;
        while file < sizes.len() {
            let size = sizes[file];
            first_chunk.push(chunk_count);
            let chunks = chunks_in(size, u64::from(chunk_size));
            match (
                total_size.checked_add(size),
                chunk_count.checked_add(chunks),
            ) {
                (Some(total), Some(count)) => {
                    total_size = total;
                    chunk_count = count;
                }
                _ => return None,
            }
            file += 1;
        }
        Some(Self {
            chunk_size,
            sizes: sizes.to_vec(),
            first_chunk,
            chunk_count,
            total_size,
        })
    }

    /// Number of chunks across all files.
    pub fn chunk_count(&self) -> u64 {
        self.chunk_count
    }

    /// Sum of all file sizes, in bytes.
    pub fn total_size(&self) -> u64 {
        self.total_size
    }

    /// The numbers of the chunks of one file: the first, and one past the
    /// last. `file` must be the index of a file.
    pub fn file_chunks(&self, file: usize) -> (u64, u64) {
        let start = self.first_chunk[file];
        let count = chunks_in(self.sizes[file], u64::from(self.chunk_size));
        (start, start + count)
    }

    /// Where chunk `index` lives, or `None` if it is out of range.
    pub fn locate(&self, index: u64) -> Option<ChunkLocation> {
        if index >= self.chunk_count {
            return None;
        }
        // Find the last file whose first chunk is at or before `index`.
        // Empty files own no chunks and share a first chunk with the file
        // after them, which is why it has to be the last such file.
        let mut low = 0;
        let mut high = self.first_chunk.len();
        while high - low > 1 {
            let middle = low + (high - low) / 2;
            if self.first_chunk[middle] <= index {
                low = middle;
            } else {
                high = middle;
            }
        }
        let file = low;

        let chunk_size = u64::from(self.chunk_size);
        let offset = (index - self.first_chunk[file]) * chunk_size;
        let remaining = self.sizes[file] - offset;
        let len = if remaining < chunk_size {
            // Shorter than a chunk, so it fits in the chunk size's type.
            to_u32(remaining)
        } else {
            self.chunk_size
        };
        Some(ChunkLocation { file, offset, len })
    }
}

/// Converts a length that is known to be below some `u32`. A value that does
/// not fit is clamped rather than truncated.
fn to_u32(n: u64) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lays_out_files_including_empty_ones() {
        let layout = Layout::new(&[25, 0, 10, 0], 10).unwrap();
        assert_eq!(layout.chunk_count(), 4);
        assert_eq!(layout.total_size(), 35);
        assert_eq!(layout.file_chunks(0), (0, 3));
        assert_eq!(layout.file_chunks(1), (3, 3));
        assert_eq!(layout.file_chunks(2), (3, 4));
        assert_eq!(layout.file_chunks(3), (4, 4));

        let at = |index| layout.locate(index).unwrap();
        assert_eq!((at(0).file, at(0).offset, at(0).len), (0, 0, 10));
        assert_eq!((at(2).file, at(2).offset, at(2).len), (0, 20, 5));
        assert_eq!((at(3).file, at(3).offset, at(3).len), (2, 0, 10));
        assert_eq!(layout.locate(4), None);
    }

    #[test]
    fn no_files_means_no_chunks() {
        let layout = Layout::new(&[], 10).unwrap();
        assert_eq!(layout.chunk_count(), 0);
        assert_eq!(layout.locate(0), None);
    }

    #[test]
    fn refuses_a_zero_chunk_size_and_sizes_that_overflow() {
        assert_eq!(Layout::new(&[1], 0), None);
        assert_eq!(Layout::new(&[u64::MAX, 1], 10), None);
        assert!(Layout::new(&[u64::MAX], 10).is_some());
    }
}

/// Bounded proofs, run by `just kani`. They hold for every chunk size, for
/// `FILES` files of up to `MAX_FILE_CHUNKS` chunks each, and for every chunk
/// index.
#[cfg(kani)]
mod proofs {
    use super::*;

    const FILES: usize = 2;
    /// Enough for an empty file, a file of one chunk, and a file whose last
    /// chunk follows a full one.
    const MAX_FILE_CHUNKS: u64 = 2;

    fn any_layout() -> (Layout, [u64; FILES], u32) {
        let chunk_size: u32 = kani::any_where(|&size| size != 0);
        let sizes: [u64; FILES] = kani::any();
        let mut file = 0;
        while file < FILES {
            kani::assume(sizes[file] <= MAX_FILE_CHUNKS * u64::from(chunk_size));
            file += 1;
        }
        let Some(layout) = Layout::new(&sizes, chunk_size) else {
            unreachable!("sizes this small cannot overflow");
        };
        (layout, sizes, chunk_size)
    }

    /// Every index in range names a chunk that lies wholly inside one file,
    /// starts on a chunk boundary and is full unless it is that file's last.
    /// Every index out of range names nothing.
    #[kani::proof]
    #[kani::unwind(4)]
    fn every_chunk_lies_inside_its_file() {
        let (layout, sizes, chunk_size) = any_layout();
        let index: u64 = kani::any();

        match layout.locate(index) {
            None => assert!(index >= layout.chunk_count()),
            Some(chunk) => {
                assert!(index < layout.chunk_count());
                assert!(chunk.file < FILES);
                let size = sizes[chunk.file];
                let end = chunk.offset + u64::from(chunk.len);

                assert!(chunk.len > 0 && chunk.len <= chunk_size);
                assert!(end <= size);
                assert!(chunk.len == chunk_size || end == size);

                // It is the right chunk of that file: the one whose position
                // in the file matches its offset.
                let (start, end) = layout.file_chunks(chunk.file);
                assert!(start <= index && index < end);
                assert!(chunk.offset == (index - start) * u64::from(chunk_size));
            }
        }
    }

    /// The files' chunk ranges follow one another with no gap or overlap and
    /// together make up every index. The total size is the sum of the files.
    #[kani::proof]
    #[kani::unwind(4)]
    fn the_files_split_the_chunks_between_them() {
        let (layout, sizes, _) = any_layout();
        let mut next = 0;
        let mut total = 0;
        let mut file = 0;
        while file < FILES {
            let (start, end) = layout.file_chunks(file);
            assert!(start == next && start <= end);
            next = end;
            total += sizes[file];
            file += 1;
        }
        assert!(next == layout.chunk_count());
        assert!(total == layout.total_size());
    }
}
