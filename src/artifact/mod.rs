//! The data model: content hashes, manifests and chunk layout.
//!
//! Nothing in this module touches the disk or the network.

mod bitfield;
mod hash;
mod manifest;

pub use bitfield::Bitfield;

/// Converts a chunk index or count into an in-memory index.
///
/// A value too large for this platform is clamped rather than truncated, so
/// that using it fails as out of range instead of wrapping round to some
/// other, valid index.
pub(crate) fn to_usize(n: u64) -> usize {
    usize::try_from(n).unwrap_or(usize::MAX)
}

pub use hash::{Hash, InvalidHash};
pub use manifest::{
    Artifact, ChunkLocation, DEFAULT_CHUNK_SIZE, FileEntry, Kind, MAX_CHUNK_SIZE,
    MAX_MANIFEST_BYTES, MIN_CHUNK_SIZE, Manifest, is_safe_component,
};
