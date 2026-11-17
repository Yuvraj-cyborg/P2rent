//! The library's error type.

use thiserror::Error;

/// A result whose error defaults to [`enum@Error`].
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Everything that can go wrong in the library.
#[derive(Debug, Error)]
pub enum Error {
    /// A file or socket operation failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// The place being read or written failed. Its kind says what can be
    /// done about it.
    #[error(transparent)]
    Place(#[from] model::place::Failure),

    /// A manifest broke one of its invariants. The text says which.
    #[error("invalid manifest: {0}")]
    InvalidManifest(String),

    /// A ticket could not be parsed, or names no peer that can be used.
    #[error("invalid ticket: {0}")]
    InvalidTicket(String),

    /// A location could not be read, or this build cannot use it. The text
    /// is a whole sentence that says what to type instead.
    #[error("{0}")]
    InvalidLocation(String),

    /// A node key or certificate could not be read, created or used.
    #[error("identity: {0}")]
    Identity(String),

    /// TLS could not be configured.
    #[error(transparent)]
    Tls(#[from] rustls::Error),

    /// A connection or stream failed.
    #[error("transport: {0}")]
    Transport(String),

    /// A peer sent something the protocol does not allow.
    #[error("protocol: {0}")]
    Protocol(String),

    /// The peer that answered is not the node that was dialled.
    #[error("peer presented node id {actual}, expected {expected}")]
    IdentityMismatch {
        /// The node id that was dialled.
        expected: String,
        /// The node id the peer proved.
        actual: String,
    },

    /// No peer supplied a manifest matching the artifact id.
    #[error("no reachable peer could provide the manifest")]
    ManifestUnavailable,

    /// No chunk arrived for the whole of the stall timeout.
    #[error("download stalled: no peer made progress for {0:?}")]
    Stalled(std::time::Duration),
}

impl From<model::Error> for Error {
    fn from(e: model::Error) -> Self {
        match e {
            model::Error::InvalidManifest(reason) => Error::InvalidManifest(reason),
            model::Error::InvalidTicket(reason) => Error::InvalidTicket(reason),
            model::Error::InvalidLocation(reason) => Error::InvalidLocation(reason),
        }
    }
}

impl From<engine::Error> for Error {
    fn from(e: engine::Error) -> Self {
        match e {
            engine::Error::Stalled(idle) => Error::Stalled(idle),
            engine::Error::Changed(what) => Error::Place(model::place::Failure::Changed(what)),
            engine::Error::Place(failure) => Error::Place(failure),
        }
    }
}

impl From<fs::Error> for Error {
    fn from(e: fs::Error) -> Self {
        match e {
            fs::Error::Io(e) => Error::Io(e),
            fs::Error::Model(e) => e.into(),
        }
    }
}

impl From<rcgen::Error> for Error {
    fn from(e: rcgen::Error) -> Self {
        Error::Identity(e.to_string())
    }
}

macro_rules! transport_errors {
    ($($source:ty),* $(,)?) => {$(
        impl From<$source> for Error {
            fn from(e: $source) -> Self {
                Error::Transport(e.to_string())
            }
        }
    )*};
}

transport_errors!(
    quinn::ConnectError,
    quinn::ConnectionError,
    quinn::WriteError,
    quinn::ReadExactError,
    quinn::ClosedStream,
);
