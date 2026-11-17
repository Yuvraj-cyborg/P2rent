//! The data model: content hashes, manifests and chunk layout.
//!
//! Nothing in this module touches the disk or the network.

mod bitfield;
mod digest;
mod hash;
mod layout;
mod manifest;
pub mod path;
mod wire;

pub use bitfield::Bitfield;
pub use digest::FileDigest;
pub use hash::{Hash, InvalidHash};
pub use layout::{ChunkLocation, Layout};
pub use manifest::{
    Artifact, Content, DEFAULT_CHUNK_SIZE, Entry, EntryKind, EntryMetadata, FORMAT_VERSION,
    MAX_CHUNK_SIZE, MAX_MANIFEST_BYTES, MAX_NAME_BYTES, MAX_TARGET_BYTES, MIN_CHUNK_SIZE, Manifest,
    Metadata, PART_SUFFIX, PERMISSION_BITS, Timestamp, Tree, is_part_name, is_safe_component,
};

/// Converts a chunk index or count into an in-memory index.
///
/// A value too large for this platform is clamped rather than truncated, so
/// that using it fails as out of range instead of wrapping round to some
/// other, valid index.
pub fn to_usize(n: u64) -> usize {
    usize::try_from(n).unwrap_or(usize::MAX)
}
