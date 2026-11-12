//! The data model: content hashes, manifests and chunk layout.
//!
//! Nothing in this module touches the disk or the network.

mod bitfield;
mod hash;
mod manifest;

pub use bitfield::Bitfield;
pub use hash::{Hash, InvalidHash};
pub use manifest::{
    Artifact, ChunkLocation, DEFAULT_CHUNK_SIZE, FileEntry, Kind, MAX_CHUNK_SIZE,
    MAX_MANIFEST_BYTES, MIN_CHUNK_SIZE, Manifest, is_safe_component,
};
