use super::Hash;
use crate::hex;
use alloc::format;
use alloc::string::String;
use core::fmt;
use core::str::FromStr;

/// The digest of a whole file, as a person or a server gives it: the kind
/// of digest, then its bytes.
///
/// It is written `blake3:<64 hex characters>` or `sha256:<64 hex
/// characters>`. It says nothing about chunks, so it can only be checked
/// once every byte of the file has arrived.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileDigest {
    /// BLAKE3 of the file's bytes, as `b3sum` prints it.
    Blake3(Hash),
    /// SHA-256 of the file's bytes, as `sha256sum` prints it.
    Sha256([u8; 32]),
}

impl FileDigest {
    /// The kind of digest, as it is written before the colon.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Blake3(_) => "blake3",
            Self::Sha256(_) => "sha256",
        }
    }

    /// The 32 bytes of the digest.
    pub fn bytes(&self) -> &[u8; 32] {
        match self {
            Self::Blake3(hash) => hash.as_bytes(),
            Self::Sha256(bytes) => bytes,
        }
    }
}

impl fmt::Display for FileDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.kind(), hex::encode(self.bytes()))
    }
}

impl FromStr for FileDigest {
    type Err = String;

    /// The error is a sentence that says what to type instead.
    fn from_str(text: &str) -> Result<Self, String> {
        const SHAPE: &str =
            "A digest is written `blake3:` or `sha256:` and then 64 hex characters.";
        let (kind, digits) = text
            .split_once(':')
            .ok_or_else(|| format!("{text:?} does not say what kind of digest it is. {SHAPE}"))?;
        let bytes = hex::decode_32(digits)
            .ok_or_else(|| format!("{digits:?} is not 64 hex characters. {SHAPE}"))?;
        match kind.to_ascii_lowercase().as_str() {
            "blake3" => Ok(Self::Blake3(Hash::from_bytes(bytes))),
            "sha256" => Ok(Self::Sha256(bytes)),
            _ => Err(format!(
                "{kind:?} is not a kind of digest mux checks. {SHAPE}"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_digest_prints_as_its_kind_and_its_hex() {
        let blake3 = FileDigest::Blake3(Hash::from_bytes([0xab; 32]));
        assert_eq!(blake3.to_string(), format!("blake3:{}", "ab".repeat(32)));
        let sha256 = FileDigest::Sha256([0x01; 32]);
        assert_eq!(sha256.to_string(), format!("sha256:{}", "01".repeat(32)));
        for digest in [blake3, sha256] {
            assert_eq!(digest.to_string().parse(), Ok(digest));
        }
        assert_eq!(blake3.bytes(), &[0xab; 32]);
        assert_eq!(sha256.kind(), "sha256");
    }

    #[test]
    fn the_kind_may_be_in_capitals_and_the_hex_too() {
        let text = format!("SHA256:{}", "AB".repeat(32));
        assert_eq!(text.parse(), Ok(FileDigest::Sha256([0xab; 32])));
    }

    #[test]
    fn what_is_not_a_digest_says_what_one_looks_like() {
        for (text, hint) in [
            ("abcd", "does not say what kind"),
            ("md5:abcd", "not 64 hex characters"),
            ("sha256:zz", "not 64 hex characters"),
            (&format!("md5:{}", "ab".repeat(32)), "not a kind of digest"),
            (
                &format!("sha256:{}", "ab".repeat(31)),
                "not 64 hex characters",
            ),
        ] {
            let refused = text.parse::<FileDigest>().unwrap_err();
            assert!(refused.contains(hint), "{text}: {refused}");
            assert!(refused.contains("`blake3:` or `sha256:`"), "{refused}");
        }
    }
}
