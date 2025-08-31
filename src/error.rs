use rand_core::OsError;
use thiserror::Error;
#[derive(Debug, Error)]
pub enum SyncError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("WalkDir error: {0}")]
    WalkDir(#[from] walkdir::Error),

    #[error("Serde error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("Base64 error: {0}")]
    Base64(#[from] base64::DecodeError),

    #[error("Key error: {0}")]
    Key(#[from] ed25519_dalek::SignatureError),

    #[error("Slice conversion error: {0}")]
    Slice(#[from] std::array::TryFromSliceError),

    #[error("Other: {0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, SyncError>;
impl From<OsError> for SyncError {
    fn from(err: OsError) -> Self {
        SyncError::Other(format!("OS RNG error: {}", err))
    }
}
