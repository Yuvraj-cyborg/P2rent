use thiserror::Error;

#[derive(Debug, Error)]
pub enum SyncError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("WalkDir error: {0}")]
    WalkDir(#[from] walkdir::Error),

    #[error("Serde JSON error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("Base64 decode error: {0}")]
    Base64(#[from] base64::DecodeError),

    #[error("Cryptography signature error: {0}")]
    Key(#[from] ed25519_dalek::SignatureError),

    #[error("Slice conversion error: {0}")]
    Slice(#[from] std::array::TryFromSliceError),

    #[error("QUIC connection error: {0}")]
    QuicConnection(#[from] quinn::ConnectionError),

    #[error("QUIC connection setup error: {0}")]
    QuicConnect(#[from] quinn::ConnectError),

    #[error("QUIC stream write error: {0}")]
    QuicWrite(#[from] quinn::WriteError),

    #[error("QUIC stream read-to-end error: {0}")]
    QuicReadToEnd(#[from] quinn::ReadToEndError),

    #[error("QUIC stream was closed: {0}")]
    QuicStreamClosed(#[from] quinn::ClosedStream),

    #[error("TLS error: {0}")]
    Tls(#[from] quinn::rustls::Error),

    #[error("Certificate generation error: {0}")]
    Rcgen(#[from] rcgen::Error),

    #[error("UTF8 conversion error: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),

    #[error("Other error: {0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, SyncError>;
