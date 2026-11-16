//! This crate's error type.

use thiserror::Error;

/// A result whose error defaults to [`enum@Error`].
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// What can go wrong when reading or describing local files.
#[derive(Debug, Error)]
pub enum Error {
    /// A file or directory operation failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// The files could not be described by a valid manifest.
    #[error(transparent)]
    Model(#[from] model::Error),
}
