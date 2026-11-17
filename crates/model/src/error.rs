//! The data model's error type.

use alloc::string::String;
use thiserror::Error;

/// A result whose error defaults to [`enum@Error`].
pub type Result<T, E = Error> = core::result::Result<T, E>;

/// What can be wrong with the data a peer, a disk or a person hands us.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum Error {
    /// A manifest broke one of its invariants. The text says which.
    #[error("invalid manifest: {0}")]
    InvalidManifest(String),

    /// A ticket could not be read. The text says what is wrong with it.
    #[error("invalid ticket: {0}")]
    InvalidTicket(String),

    /// A location could not be read, or this build cannot use it. The text
    /// is a whole sentence that says what to type instead.
    #[error("{0}")]
    InvalidLocation(String),
}
