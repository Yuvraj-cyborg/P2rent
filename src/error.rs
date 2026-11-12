use thiserror::Error;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),

    #[error("invalid manifest: {0}")]
    InvalidManifest(String),

    #[error("invalid ticket: {0}")]
    InvalidTicket(String),

    #[error("identity: {0}")]
    Identity(String),

    #[error("tls: {0}")]
    Tls(#[from] rustls::Error),

    #[error("transport: {0}")]
    Transport(String),

    #[error("protocol: {0}")]
    Protocol(String),

    #[error("peer presented node id {actual}, expected {expected}")]
    IdentityMismatch { expected: String, actual: String },

    #[error("no reachable peer could provide the manifest")]
    ManifestUnavailable,

    #[error("download stalled: no peer made progress for {0:?}")]
    Stalled(std::time::Duration),
}
