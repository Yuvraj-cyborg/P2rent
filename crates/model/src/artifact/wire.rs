//! How the data model is written to the wire.
//!
//! The encodings live here, apart from the types they encode, so that the
//! modules holding provable logic hold nothing else.

use super::Bitfield;
use alloc::vec::Vec;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A bitfield as sent: its length in bits, then its bytes.
#[derive(Serialize)]
struct BitfieldRef<'a> {
    len: u64,
    bytes: &'a [u8],
}

/// A bitfield as received.
#[derive(Deserialize)]
struct BitfieldOwned {
    len: u64,
    bytes: Vec<u8>,
}

impl Serialize for Bitfield {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        BitfieldRef {
            len: self.len(),
            bytes: self.bytes(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Bitfield {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = BitfieldOwned::deserialize(deserializer)?;
        Ok(Bitfield::from_parts(wire.len, wire.bytes))
    }
}
