//! The data model: content hashes, manifests and chunk layout.
//!
//! Nothing in this module touches the disk or the network.

mod bitfield;
mod hash;
mod layout;
mod manifest;
mod wire;

pub use bitfield::Bitfield;
pub use hash::{Hash, InvalidHash};
pub use layout::{ChunkLocation, Layout};
pub use manifest::{
    Artifact, DEFAULT_CHUNK_SIZE, FileEntry, Kind, MAX_CHUNK_SIZE, MAX_COMPONENT_BYTES,
    MAX_MANIFEST_BYTES, MIN_CHUNK_SIZE, Manifest, PART_SUFFIX, is_part_name, is_safe_component,
};

/// Converts a chunk index or count into an in-memory index.
///
/// A value too large for this platform is clamped rather than truncated, so
/// that using it fails as out of range instead of wrapping round to some
/// other, valid index.
pub fn to_usize(n: u64) -> usize {
    usize::try_from(n).unwrap_or(usize::MAX)
}
