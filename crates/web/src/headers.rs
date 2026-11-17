//! What a web server says about a file, read out of its headers.
//!
//! Every function here takes text that came from a server and may be
//! anything. None of them can panic, and what cannot be read is treated as
//! not having been said.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use model::artifact::{FileDigest, Timestamp};
use std::time::UNIX_EPOCH;

/// The range a response carries, as `Content-Range` gives it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentRange {
    /// The position of the first byte.
    pub first: u64,
    /// The position of the last byte.
    pub last: u64,
    /// The size of the whole file, if the server says.
    pub total: Option<u64>,
}

/// Reads `bytes 0-99/1234` or `bytes 0-99/*`.
pub fn content_range(value: &str) -> Option<ContentRange> {
    let (range, total) = value.trim().strip_prefix("bytes ")?.split_once('/')?;
    let (first, last) = range.trim().split_once('-')?;
    let (first, last) = (number(first)?, number(last)?);
    let total = match total.trim() {
        "*" => None,
        total => Some(number(total)?),
    };
    let fits = first <= last && total.is_none_or(|total| last < total);
    fits.then_some(ContentRange { first, last, total })
}

/// Reads the size out of `bytes */1234`, which a server sends to say that a
/// range lies outside the file.
pub fn unsatisfied_total(value: &str) -> Option<u64> {
    number(
        value
            .trim()
            .strip_prefix("bytes ")?
            .trim()
            .strip_prefix("*/")?,
    )
}

/// A count of bytes: digits and nothing else.
fn number(text: &str) -> Option<u64> {
    let text = text.trim();
    let digits = !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
    digits.then(|| text.parse().ok())?
}

/// What a server gives to tell one version of a file from another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Validator {
    /// An entity tag that changes whenever the bytes do.
    ETag(String),
    /// The time the file was last changed, as the server wrote it.
    Date(String),
}

impl Validator {
    /// The validator a server gives: a strong entity tag if it has one,
    /// and otherwise the time the file was last changed. A weak entity tag
    /// says that two versions are alike enough, not that they are the same
    /// bytes, so it is no use here.
    pub fn from(etag: Option<&str>, last_modified: Option<&str>) -> Option<Self> {
        let strong = etag.map(str::trim).filter(|tag| {
            tag.len() >= 2 && tag.starts_with('"') && tag.ends_with('"') && tag.is_ascii()
        });
        let date = last_modified
            .map(str::trim)
            .filter(|date| httpdate::parse_http_date(date).is_ok());
        strong
            .map(|tag| Self::ETag(tag.to_owned()))
            .or_else(|| date.map(|date| Self::Date(date.to_owned())))
    }

    /// The validator as it is sent back to the server in `If-Range`.
    pub fn as_header(&self) -> &str {
        match self {
            Self::ETag(text) | Self::Date(text) => text,
        }
    }

    /// The validator as bytes that differ whenever the version does, which
    /// is what a transfer notes beside a file.
    pub fn to_bytes(&self) -> Vec<u8> {
        let kind = match self {
            Self::ETag(_) => "etag:",
            Self::Date(_) => "date:",
        };
        [kind, self.as_header()].concat().into_bytes()
    }

    /// Whether a later response is of the same version: it must repeat
    /// what this validator was read from.
    pub fn still_holds(&self, etag: Option<&str>, last_modified: Option<&str>) -> bool {
        match self {
            Self::ETag(tag) => etag.map(str::trim) == Some(tag),
            Self::Date(date) => last_modified.map(str::trim) == Some(date),
        }
    }
}

/// The time in `Last-Modified`, if it is a time.
pub fn modified(last_modified: &str) -> Option<Timestamp> {
    let time = httpdate::parse_http_date(last_modified.trim()).ok()?;
    let since = time.duration_since(UNIX_EPOCH).ok()?;
    Some(Timestamp {
        seconds: i64::try_from(since.as_secs()).ok()?,
        nanos: since.subsec_nanos(),
    })
}

/// The SHA-256 of the whole file, if the server gives it: in `Repr-Digest`
/// as `sha-256=:<base64>:`, or in the older `Digest` as `sha-256=<base64>`.
/// Either may list other digests beside it, which are passed over.
pub fn sha256(repr_digest: Option<&str>, digest: Option<&str>) -> Option<FileDigest> {
    let listed = |header: &str, wrapped: bool| {
        header.split(',').find_map(|item| {
            let (kind, value) = item.trim().split_once('=')?;
            if !kind.trim().eq_ignore_ascii_case("sha-256") {
                return None;
            }
            let value = value.trim();
            let encoded = if wrapped {
                value.strip_prefix(':')?.strip_suffix(':')?
            } else {
                value
            };
            let bytes: [u8; 32] = STANDARD.decode(encoded).ok()?.try_into().ok()?;
            Some(FileDigest::Sha256(bytes))
        })
    };
    repr_digest
        .and_then(|header| listed(header, true))
        .or_else(|| digest.and_then(|header| listed(header, false)))
}

/// The name to store a file under, from the path of its address: the last
/// part of the path, with `%XX` read as the byte it stands for. `None` if
/// that is not a name a file can safely have.
pub fn name_from(path: &str) -> Option<String> {
    let last = path.rsplit('/').find(|part| !part.is_empty())?;
    let mut bytes = Vec::with_capacity(last.len());
    let mut rest = last.as_bytes();
    while let Some((&byte, after)) = rest.split_first() {
        let escaped = (byte == b'%')
            .then(|| after.split_first_chunk::<2>())
            .flatten()
            .and_then(|(pair, after)| Some((hex(pair[0])? * 16 + hex(pair[1])?, after)));
        let (byte, after) = escaped.unwrap_or((byte, after));
        bytes.push(byte);
        rest = after;
    }
    let name = String::from_utf8(bytes).ok()?;
    model::artifact::is_safe_component(&name).then_some(name)
}

fn hex(digit: u8) -> Option<u8> {
    char::from(digit)
        .to_digit(16)
        .and_then(|value| u8::try_from(value).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_content_range_says_where_the_bytes_lie() {
        let range = |first, last, total| Some(ContentRange { first, last, total });
        assert_eq!(content_range("bytes 0-0/1234"), range(0, 0, Some(1234)));
        assert_eq!(
            content_range("bytes 100-1233/1234"),
            range(100, 1233, Some(1234))
        );
        assert_eq!(content_range(" bytes 5-9/* "), range(5, 9, None));
        assert_eq!(content_range("bytes 5 - 9 / 10"), range(5, 9, Some(10)));
    }

    #[test]
    fn a_content_range_that_makes_no_sense_is_not_read() {
        for wrong in [
            "",
            "bytes",
            "bytes 0-0",
            "items 0-0/10",
            "bytes 9-5/10",
            "bytes 0-10/10",
            "bytes -5/10",
            "bytes 0-/10",
            "bytes a-b/c",
            "bytes +1-2/3",
            "bytes 0-0/-1",
            "bytes */10",
            "bytes 0-99999999999999999999/100000000000000000000",
        ] {
            assert_eq!(content_range(wrong), None, "{wrong:?}");
        }
    }

    #[test]
    fn an_unsatisfied_range_still_gives_the_size() {
        assert_eq!(unsatisfied_total("bytes */0"), Some(0));
        assert_eq!(unsatisfied_total(" bytes */1234 "), Some(1234));
        for wrong in ["bytes 0-0/10", "bytes */*", "*/10", "bytes */-1", ""] {
            assert_eq!(unsatisfied_total(wrong), None, "{wrong:?}");
        }
    }

    const DATE: &str = "Tue, 17 Nov 2026 10:00:00 GMT";

    #[test]
    fn a_strong_entity_tag_is_the_validator_and_a_date_is_the_next_best() {
        let tag = Validator::from(Some(" \"abc\" "), Some(DATE)).unwrap();
        assert_eq!(tag, Validator::ETag("\"abc\"".into()));
        assert_eq!(tag.as_header(), "\"abc\"");
        assert_eq!(tag.to_bytes(), b"etag:\"abc\"");

        let date = Validator::from(None, Some(DATE)).unwrap();
        assert_eq!(date, Validator::Date(DATE.into()));
        assert_eq!(date.to_bytes(), format!("date:{DATE}").into_bytes());
        // A weak tag is passed over for the date.
        assert_eq!(Validator::from(Some("W/\"abc\""), Some(DATE)), Some(date));
    }

    #[test]
    fn what_cannot_tell_versions_apart_is_no_validator() {
        for (etag, date) in [
            (None, None),
            (Some("W/\"abc\""), None),
            (Some("abc"), None),
            (Some("\""), None),
            (Some("\"\u{e9}t\u{e9}\""), None),
            (None, Some("yesterday")),
            (Some(""), Some("")),
        ] {
            assert_eq!(Validator::from(etag, date), None, "{etag:?} {date:?}");
        }
    }

    #[test]
    fn a_validator_holds_only_while_the_server_repeats_it() {
        let tag = Validator::ETag("\"abc\"".into());
        assert!(tag.still_holds(Some("\"abc\""), None));
        assert!(tag.still_holds(Some(" \"abc\" "), Some("anything")));
        assert!(!tag.still_holds(Some("\"xyz\""), None));
        assert!(!tag.still_holds(None, Some(DATE)));

        let date = Validator::Date(DATE.into());
        assert!(date.still_holds(Some("\"new\""), Some(DATE)));
        assert!(!date.still_holds(None, Some("Tue, 17 Nov 2026 10:00:01 GMT")));
        assert!(!date.still_holds(None, None));
    }

    #[test]
    fn the_time_a_file_was_changed_is_read_as_seconds() {
        let read = modified(DATE).unwrap();
        assert_eq!((read.seconds, read.nanos), (1_794_909_600, 0));
        assert_eq!(
            modified("Thu, 01 Jan 1970 00:00:00 GMT").unwrap().seconds,
            0
        );
        assert_eq!(modified("not a date"), None);
    }

    #[test]
    fn a_sha256_is_read_from_either_digest_header() {
        // The SHA-256 of nothing.
        let encoded = "47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=";
        let empty = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let expected: FileDigest = format!("sha256:{empty}").parse().unwrap();

        let repr = format!("sha-256=:{encoded}:");
        assert_eq!(sha256(Some(&repr), None), Some(expected));
        let among = format!("md5=:abc:, SHA-256=:{encoded}: , sha-512=:xyz:");
        assert_eq!(sha256(Some(&among), None), Some(expected));
        let legacy = format!("MD5=abc,SHA-256={encoded}");
        assert_eq!(sha256(None, Some(&legacy)), Some(expected));
        // The newer header is believed first.
        assert_eq!(sha256(Some(&repr), Some("sha-256=AAAA")), Some(expected));
    }

    #[test]
    fn a_digest_that_is_not_a_sha256_is_not_read() {
        let encoded = "47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=";
        for (repr, legacy) in [
            (None, None),
            (Some("sha-512=:abc:"), None),
            (Some("sha-256=abc"), None),
            (Some("sha-256=::"), None),
            (Some("sha-256=:AAAA:"), None),
            (Some("sha-256=:not base64!:"), None),
            (None, Some("sha-256=AAAA")),
            (None, Some("sha-256")),
            (None, Some("")),
        ] {
            assert_eq!(sha256(repr, legacy), None, "{repr:?} {legacy:?}");
        }
        // Without its colons it is not the newer header's form.
        assert_eq!(sha256(Some(&format!("sha-256={encoded}")), None), None);
    }

    #[test]
    fn a_file_is_named_for_the_last_part_of_its_address() {
        for (path, name) in [
            ("/big.iso", "big.iso"),
            ("/a/b/model.safetensors", "model.safetensors"),
            ("/a/b/dir/", "dir"),
            ("/a%20b.txt", "a b.txt"),
            ("/%C3%A9t%C3%A9", "\u{e9}t\u{e9}"),
            ("/100%", "100%"),
            ("/a%zzb", "a%zzb"),
            ("plain", "plain"),
        ] {
            assert_eq!(name_from(path).as_deref(), Some(name), "{path}");
        }
    }

    #[test]
    fn an_address_that_gives_no_safe_name_gives_none() {
        for path in [
            "",
            "/",
            "//",
            "/..",
            "/.",
            "/a%2Fb",
            "/a%5Cb",
            "/%00",
            "/%FF",
            "/c%3Astream",
            "/file.mux-part",
        ] {
            assert_eq!(name_from(path), None, "{path:?}");
        }
    }
}
