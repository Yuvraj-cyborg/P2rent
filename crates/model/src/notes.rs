//! What a transfer writes down about a file whose digests were not known
//! when it began.
//!
//! The digest of each chunk is noted as the chunk is stored, beside the
//! file being written. When the transfer is run again it reads the notes,
//! and a chunk whose bytes still have the digest noted for it is not
//! fetched again. The notes begin with what tells one version of the file
//! from another, so notes about another version are not believed.
//!
//! Notes are read back from a disk, where they may have been cut short or
//! damaged. Nothing in them is trusted: a digest that was noted wrongly
//! only means that its chunk is fetched again.
//!
//! ```text
//! "mux-note"  version  chunk size  file size  validator length  validator
//!     8          1         4           8             2              n
//! then, for each chunk in order:   1 if a digest follows, else 0   digest
//!                                              1                     32
//! ```
//!
//! Numbers are little-endian.

use crate::artifact::Hash;
use alloc::vec::Vec;

const MAGIC: &[u8; 8] = b"mux-note";
const VERSION: u8 = 1;
/// The bytes before the validator.
const FIXED_BYTES: usize = 8 + 1 + 4 + 8 + 2;
/// One chunk's place in the table: whether a digest is there, then the
/// digest.
const SLOT_BYTES: usize = 1 + 32;

/// The longest validator that notes hold.
pub const MAX_VALIDATOR_BYTES: usize = 1024;

/// The notes about one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notes {
    size: u64,
    chunk_size: u32,
    validator: Vec<u8>,
    /// The digest noted for each chunk, for as many chunks as the notes
    /// reach.
    digests: Vec<Option<Hash>>,
}

impl Notes {
    /// Notes with no digest in them yet, for a file of `size` bytes cut
    /// into chunks of `chunk_size`, in the version that `validator` stands
    /// for. `None` if there is no validator or it is too long: then nothing
    /// could tell these notes from those of another version, and none are
    /// kept.
    pub fn new(size: u64, chunk_size: u32, validator: &[u8]) -> Option<Self> {
        let usable = !validator.is_empty() && validator.len() <= MAX_VALIDATOR_BYTES;
        usable.then(|| Self {
            size,
            chunk_size,
            validator: validator.to_vec(),
            digests: Vec::new(),
        })
    }

    /// The most bytes the notes of a file can take, for a file of `size`
    /// bytes cut into chunks of `chunk_size`: enough to read back, and no
    /// reason to read more.
    pub fn longest(size: u64, chunk_size: u32) -> usize {
        let chunks = size.div_ceil(u64::from(chunk_size.max(1)));
        usize::try_from(chunks)
            .unwrap_or(usize::MAX)
            .saturating_mul(SLOT_BYTES)
            .saturating_add(FIXED_BYTES + MAX_VALIDATOR_BYTES)
    }

    /// The bytes the notes begin with, to be written at their start.
    pub fn header(&self) -> Vec<u8> {
        let mut header = Vec::with_capacity(self.table_start());
        header.extend_from_slice(MAGIC);
        header.push(VERSION);
        header.extend_from_slice(&self.chunk_size.to_le_bytes());
        header.extend_from_slice(&self.size.to_le_bytes());
        // The length was checked when the notes were made or read.
        let length = u16::try_from(self.validator.len()).unwrap_or(u16::MAX);
        header.extend_from_slice(&length.to_le_bytes());
        header.extend_from_slice(&self.validator);
        header
    }

    /// Where in the notes the digest of chunk `chunk` of the file goes, and
    /// the bytes to write there. `None` if that place is beyond what a
    /// number can hold.
    pub fn slot(&self, chunk: u64, digest: Hash) -> Option<(u64, [u8; SLOT_BYTES])> {
        let offset = chunk
            .checked_mul(SLOT_BYTES as u64)?
            .checked_add(self.table_start() as u64)?;
        let mut bytes = [1; SLOT_BYTES];
        bytes[1..].copy_from_slice(digest.as_bytes());
        Some((offset, bytes))
    }

    /// Reads notes back. `None` if the bytes are not notes this build
    /// reads. Notes that were cut short say nothing about the chunks they
    /// do not reach.
    pub fn read(bytes: &[u8]) -> Option<Self> {
        let (fixed, rest) = bytes.split_at_checked(FIXED_BYTES)?;
        let (magic, fixed) = fixed.split_at(MAGIC.len());
        let (&version, fixed) = fixed.split_first()?;
        if magic != MAGIC || version != VERSION {
            return None;
        }
        let (chunk_size, fixed) = fixed.split_at(4);
        let (size, length) = fixed.split_at(8);
        let chunk_size = u32::from_le_bytes(chunk_size.try_into().ok()?);
        let size = u64::from_le_bytes(size.try_into().ok()?);
        let length = usize::from(u16::from_le_bytes(length.try_into().ok()?));
        if chunk_size == 0 || length == 0 || length > MAX_VALIDATOR_BYTES {
            return None;
        }
        let (validator, table) = rest.split_at_checked(length)?;

        // As many slots as the file has chunks and the bytes hold.
        let chunks = size.div_ceil(u64::from(chunk_size));
        let (slots, _) = table.as_chunks::<SLOT_BYTES>();
        let digests = slots
            .iter()
            .take(usize::try_from(chunks).unwrap_or(usize::MAX))
            .map(|slot| {
                let (&noted, digest) = slot.split_first()?;
                let digest: [u8; 32] = digest.try_into().ok()?;
                (noted == 1).then(|| Hash::from_bytes(digest))
            })
            .collect();
        Some(Self {
            size,
            chunk_size,
            validator: validator.to_vec(),
            digests,
        })
    }

    /// Whether the notes are about this file in this version.
    pub fn are_about(&self, size: u64, chunk_size: u32, validator: &[u8]) -> bool {
        self.size == size && self.chunk_size == chunk_size && self.validator == validator
    }

    /// The digest noted for chunk `chunk` of the file, if one was.
    pub fn digest(&self, chunk: u64) -> Option<Hash> {
        let noted = self.digests.get(usize::try_from(chunk).ok()?)?;
        *noted
    }

    fn table_start(&self) -> usize {
        FIXED_BYTES + self.validator.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHUNK: u32 = 4096;

    /// Notes for a file of five chunks, as bytes, with the digests of
    /// `chunks` written in.
    fn written(chunks: &[u64]) -> (Notes, Vec<u8>) {
        let notes = Notes::new(u64::from(CHUNK) * 4 + 1, CHUNK, b"\"etag-1\"").unwrap();
        let mut bytes = notes.header();
        for &chunk in chunks {
            let (offset, slot) = notes.slot(chunk, digest_of(chunk)).unwrap();
            let offset = usize::try_from(offset).unwrap();
            if bytes.len() < offset + slot.len() {
                bytes.resize(offset + slot.len(), 0);
            }
            bytes[offset..offset + slot.len()].copy_from_slice(&slot);
        }
        (notes, bytes)
    }

    fn digest_of(chunk: u64) -> Hash {
        Hash::of(&chunk.to_le_bytes())
    }

    #[test]
    fn what_is_noted_is_read_back() {
        let (notes, bytes) = written(&[3, 0]);
        let read = Notes::read(&bytes).unwrap();
        assert!(read.are_about(u64::from(CHUNK) * 4 + 1, CHUNK, b"\"etag-1\""));
        assert_eq!(read.digest(0), Some(digest_of(0)));
        assert_eq!(read.digest(3), Some(digest_of(3)));
        // Chunks that were skipped over, never reached, or do not exist.
        assert_eq!(read.digest(1), None);
        assert_eq!(read.digest(4), None);
        assert_eq!(read.digest(5), None);
        assert_eq!(read.digest(u64::MAX), None);
        assert_eq!(read.header(), notes.header());
    }

    #[test]
    fn notes_with_nothing_noted_say_nothing() {
        let (_, bytes) = written(&[]);
        let read = Notes::read(&bytes).unwrap();
        assert_eq!((0..6).filter_map(|chunk| read.digest(chunk)).count(), 0);
    }

    #[test]
    fn notes_are_about_one_version_of_one_file() {
        let (notes, _) = written(&[]);
        let size = u64::from(CHUNK) * 4 + 1;
        assert!(notes.are_about(size, CHUNK, b"\"etag-1\""));
        assert!(!notes.are_about(size, CHUNK, b"\"etag-2\""));
        assert!(!notes.are_about(size + 1, CHUNK, b"\"etag-1\""));
        assert!(!notes.are_about(size, CHUNK * 2, b"\"etag-1\""));
    }

    #[test]
    fn without_a_validator_no_notes_are_kept() {
        assert!(Notes::new(1, CHUNK, b"").is_none());
        assert!(Notes::new(1, CHUNK, &[b'x'; MAX_VALIDATOR_BYTES]).is_some());
        assert!(Notes::new(1, CHUNK, &[b'x'; MAX_VALIDATOR_BYTES + 1]).is_none());
    }

    #[test]
    fn notes_cut_short_keep_what_they_reach() {
        let (_, bytes) = written(&[0, 1, 2]);
        // Cut in the middle of the third digest.
        let read = Notes::read(&bytes[..bytes.len() - 5]).unwrap();
        assert_eq!(read.digest(0), Some(digest_of(0)));
        assert_eq!(read.digest(1), Some(digest_of(1)));
        assert_eq!(read.digest(2), None);
        // Cut inside the header, they are not notes at all.
        assert!(Notes::read(&bytes[..FIXED_BYTES + 2]).is_none());
        assert!(Notes::read(&bytes[..3]).is_none());
        assert!(Notes::read(&[]).is_none());
    }

    #[test]
    fn slots_past_the_last_chunk_are_not_read() {
        let (notes, mut bytes) = written(&[4]);
        // A sixth slot, which the file has no chunk for.
        let (offset, slot) = notes.slot(5, digest_of(5)).unwrap();
        bytes.resize(usize::try_from(offset).unwrap(), 0);
        bytes.extend_from_slice(&slot);
        let read = Notes::read(&bytes).unwrap();
        assert_eq!(read.digest(4), Some(digest_of(4)));
        assert_eq!(read.digest(5), None);
    }

    #[test]
    fn what_is_not_notes_is_not_read() {
        let (_, bytes) = written(&[0]);
        let mut other_magic = bytes.clone();
        other_magic[0] ^= 1;
        let mut other_version = bytes.clone();
        other_version[MAGIC.len()] = VERSION + 1;
        let mut no_chunk_size = bytes.clone();
        no_chunk_size[9..13].fill(0);
        let mut no_validator = bytes.clone();
        no_validator[21..23].fill(0);
        let mut long_validator = bytes.clone();
        long_validator[21..23].copy_from_slice(&2000u16.to_le_bytes());
        for wrong in [
            other_magic,
            other_version,
            no_chunk_size,
            no_validator,
            long_validator,
        ] {
            assert!(Notes::read(&wrong).is_none());
        }
        // A slot whose first byte is not 1 holds no digest.
        let mut unmarked = bytes;
        let start = FIXED_BYTES + b"\"etag-1\"".len();
        unmarked[start] = 2;
        assert_eq!(Notes::read(&unmarked).unwrap().digest(0), None);
    }

    #[test]
    fn the_longest_validator_is_read_and_one_byte_more_is_not() {
        let longest = [b'x'; MAX_VALIDATOR_BYTES];
        let notes = Notes::new(1, CHUNK, &longest).unwrap();
        let read = Notes::read(&notes.header()).unwrap();
        assert!(read.are_about(1, CHUNK, &longest));

        // The same header, saying its validator is a byte longer, with the
        // byte there.
        let mut longer = notes.header();
        let length = u16::try_from(MAX_VALIDATOR_BYTES + 1).unwrap();
        longer[21..23].copy_from_slice(&length.to_le_bytes());
        longer.push(b'x');
        assert!(Notes::read(&longer).is_none());
    }

    #[test]
    fn the_longest_notes_are_a_header_and_a_slot_for_each_chunk() {
        let header = FIXED_BYTES + MAX_VALIDATOR_BYTES;
        assert_eq!(Notes::longest(0, CHUNK), header);
        assert_eq!(Notes::longest(1, CHUNK), header + 33);
        assert_eq!(Notes::longest(u64::from(CHUNK), CHUNK), header + 33);
        assert_eq!(
            Notes::longest(u64::from(CHUNK) * 4 + 1, CHUNK),
            header + 5 * 33
        );
        // Notes written with the longest validator there is fit.
        let validator = [b'x'; MAX_VALIDATOR_BYTES];
        let notes = Notes::new(u64::from(CHUNK) * 3, CHUNK, &validator).unwrap();
        let (offset, slot) = notes.slot(2, digest_of(2)).unwrap();
        let end = usize::try_from(offset).unwrap() + slot.len();
        assert_eq!(end, Notes::longest(u64::from(CHUNK) * 3, CHUNK));
        // Sizes nothing could hold do not overflow.
        assert_eq!(Notes::longest(u64::MAX, 1), usize::MAX);
        assert_eq!(Notes::longest(5, 0), header + 5 * 33);
    }

    #[test]
    fn a_slot_far_beyond_any_file_has_no_place() {
        let (notes, _) = written(&[]);
        assert!(notes.slot(u64::MAX, digest_of(0)).is_none());
        assert!(notes.slot(u64::MAX / 33, digest_of(0)).is_none());
    }
}
