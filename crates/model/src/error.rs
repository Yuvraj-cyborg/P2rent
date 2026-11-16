//! The data model's error type.

use alloc::string::String;
use thiserror::Error;

/// A result whose error defaults to [`enum@Error`].
pub type Result<T, E = Error> = core::result::Result<T, E>;

/// What can be wrong with the data a peer or a disk hands us.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum Error {
    /// A manifest broke one of its invariants. The text says which.
    #[error("invalid manifest: {0}")]
    InvalidManifest(String),
}
