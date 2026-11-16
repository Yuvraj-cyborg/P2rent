//! Minimal lowercase hex encoding for 32-byte identifiers.

use alloc::string::String;
use alloc::vec::Vec;

const DIGITS: &[u8; 16] = b"0123456789abcdef";

/// Lowercase hex, two characters per byte.
pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(DIGITS[(b >> 4) as usize] as char);
        out.push(DIGITS[(b & 0x0f) as usize] as char);
    }
    out
}

/// Decodes hex of any even length, in either case. `None` if the text is
/// not hex.
pub fn decode(text: &str) -> Option<Vec<u8>> {
    let text = text.as_bytes();
    let (pairs, rest) = text.as_chunks::<2>();
    if !rest.is_empty() {
        return None;
    }
    pairs
        .iter()
        .map(|&[hi, lo]| Some(nibble(hi)? << 4 | nibble(lo)?))
        .collect()
}

/// Decodes exactly 32 bytes of hex. `None` if the text is not 64 hex
/// characters.
pub fn decode_32(text: &str) -> Option<[u8; 32]> {
    decode(text)?.try_into().ok()
}

fn nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let bytes: Vec<u8> = (0..=255).collect();
        assert_eq!(decode(&encode(&bytes)).unwrap(), bytes);
    }

    #[test]
    fn accepts_either_case() {
        assert_eq!(decode("0a1B9fF0"), Some(vec![0x0a, 0x1b, 0x9f, 0xf0]));
        assert_eq!(decode("ABCDEF"), decode("abcdef"));
        assert_eq!(decode("AF"), Some(vec![0xaf]));
    }

    #[test]
    fn rejects_malformed_input() {
        assert!(decode("abc").is_none());
        assert!(decode("zz").is_none());
        assert!(decode_32("00").is_none());
    }
}
