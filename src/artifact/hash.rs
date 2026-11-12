use crate::hex;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// A BLAKE3 digest. Identifies chunks and, via the manifest, whole artifacts.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Hash([u8; 32]);

impl Hash {
    pub fn of(data: &[u8]) -> Self {
        Self(*blake3::hash(data).as_bytes())
    }

    /// Domain-separated digest, so a manifest can never collide with a chunk.
    pub fn derive(context: &str, data: &[u8]) -> Self {
        Self(blake3::derive_key(context, data))
    }

    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_hex(&self) -> String {
        hex::encode(&self.0)
    }

    /// First eight hex digits, for log lines.
    pub fn short(&self) -> String {
        hex::encode(&self.0[..4])
    }
}

impl fmt::Display for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Hash({})", self.short())
    }
}

impl FromStr for Hash {
    type Err = InvalidHash;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        hex::decode_32(s).map(Self).ok_or(InvalidHash)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("expected 64 hex characters")]
pub struct InvalidHash;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trip() {
        let hash = Hash::of(b"p2rent");
        assert_eq!(hash.to_hex().parse::<Hash>().unwrap(), hash);
        assert_eq!(hash.to_hex().len(), 64);
    }

    #[test]
    fn derive_is_domain_separated() {
        assert_ne!(Hash::of(b"x"), Hash::derive("p2rent test", b"x"));
    }
}
