//! Naming a place: a path, a web address or a ticket.
//!
//! Every command takes the places it works on in one written form, and this
//! module is the one reader of that form. It decides what kind of place a
//! text names and nothing more. Whether the place exists is for the code
//! that opens it.

use crate::error::{Error, Result};
use crate::hex;
use crate::ticket::Ticket;
use alloc::borrow::ToOwned;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

/// A kind of place a build of mux can use, selected by a location's scheme.
///
/// Each backend declares the protocols it provides. A build's list is that
/// of the backends compiled into it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Protocol {
    /// The scheme that selects it, as in `https://…`.
    pub scheme: &'static str,
    /// Whether data can be read from such a place.
    pub reads: bool,
    /// Whether data can be written to such a place.
    pub writes: bool,
    /// What kind of place it is, in a few words.
    pub about: &'static str,
}

/// Every scheme mux has a meaning for, with the kind of place it names and
/// the feature a build needs for it. A scheme with no feature is always
/// there, or is reserved for a place mux cannot use yet.
const KNOWN: &[(&str, &str, Option<&str>)] = &[
    ("file", "local files", None),
    ("http", "web addresses", Some("web")),
    ("https", "web addresses", Some("web")),
    ("mux", "tickets", None),
    ("peer", "serving to other machines", None),
    ("s3", "cloud storage", None),
    ("gs", "cloud storage", None),
    ("az", "cloud storage", None),
    ("sftp", "cloud storage", None),
    ("hf", "model registries", None),
    ("oci", "container registries", None),
    ("magnet", "torrents", None),
];

/// A place data can come from or go to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    /// A file or folder on this machine, as the operating system names it.
    Local(String),
    /// A web address, starting with `http://` or `https://`.
    Web(String),
    /// Content held by other machines.
    Ticket(Ticket),
    /// This machine, serving what it is given to other machines.
    Serve,
}

impl Location {
    /// Reads a location as a person types it.
    ///
    /// `protocols` lists what this build can use. A location of any other
    /// kind is refused, even when its text is well formed.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidLocation`] with a sentence that says what to
    /// type instead, or [`Error::InvalidTicket`] for a ticket that cannot be
    /// read.
    pub fn parse(text: &str, protocols: &[Protocol]) -> Result<Self> {
        if text.is_empty() {
            return Err(invalid(
                "a location is empty. Write a path, a web address or a ticket.",
            ));
        }
        if text == "-" {
            return Err(invalid(
                "standard input and output (`-`) are not supported yet. For a file named `-`, write `./-`.",
            ));
        }
        let Some((scheme, rest)) = scheme_of(text) else {
            require("file", text, protocols)?;
            return local(text);
        };
        let scheme = scheme.to_ascii_lowercase();
        require(&scheme, text, protocols)?;
        match scheme.as_str() {
            "file" => local(&file_url_path(rest)?),
            "http" | "https" => web(&scheme, rest),
            "mux" => Ok(Self::Ticket(format!("mux:{rest}").parse()?)),
            "peer" if rest.is_empty() => Ok(Self::Serve),
            "peer" => Err(invalid(format!(
                "`peer:` means this machine and takes nothing after the colon, but {text:?} has more. Write `peer:`."
            ))),
            _ => Err(invalid(format!(
                "this build lists `{scheme}:`, but mux has no rule for reading such a location."
            ))),
        }
    }
}

impl Location {
    /// The place as a message names it: as it is written, but for a ticket
    /// only as far as the id of what it names. What follows the id holds
    /// the ticket's secret, and a message is read by more people than a
    /// ticket is meant for.
    pub fn in_a_message(&self) -> String {
        match self {
            Self::Ticket(ticket) => format!("mux://{}", ticket.artifact),
            other => format!("{other}"),
        }
    }
}

impl fmt::Display for Location {
    /// Prints text that [`Location::parse`] reads back as the same place.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // A path that would read as something else is marked as a path.
            Self::Local(path) if path == "-" || scheme_of(path).is_some() => {
                write!(f, "./{path}")
            }
            Self::Local(text) | Self::Web(text) => f.write_str(text),
            Self::Ticket(ticket) => ticket.fmt(f),
            Self::Serve => f.write_str("peer:"),
        }
    }
}

/// Splits a scheme from what follows its colon. A scheme is a letter, then
/// letters, digits, `+`, `-` or `.`, and at least two characters in all:
/// one letter and a colon is a Windows drive, as in `C:\data`.
fn scheme_of(text: &str) -> Option<(&str, &str)> {
    let (scheme, rest) = text.split_once(':')?;
    let named = scheme.len() >= 2
        && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    named.then_some((scheme, rest))
}

/// Refuses a scheme this build has no protocol for, saying whether mux
/// knows the scheme at all and what the build can use.
fn require(scheme: &str, text: &str, protocols: &[Protocol]) -> Result<()> {
    if protocols.iter().any(|protocol| protocol.scheme == scheme) {
        return Ok(());
    }
    let usable = protocols
        .iter()
        .map(|protocol| protocol.scheme)
        .collect::<Vec<_>>()
        .join(", ");
    Err(match KNOWN.iter().find(|(known, _, _)| *known == scheme) {
        Some((_, place, Some(feature))) => invalid(format!(
            "this build of mux cannot use `{scheme}:` locations ({place}): it was built without the `{feature}` feature. It can use: {usable}."
        )),
        Some((_, place, None)) => invalid(format!(
            "this build of mux cannot use `{scheme}:` locations ({place}). It can use: {usable}."
        )),
        None => invalid(format!(
            "`{scheme}:` is not a kind of place mux knows. This build can use: {usable}. If {text:?} is a file, write `./{text}`."
        )),
    })
}

fn local(path: &str) -> Result<Location> {
    if path.contains('\0') {
        return Err(invalid("a path cannot contain a NUL character. Remove it."));
    }
    Ok(Location::Local(path.to_owned()))
}

/// The path a `file:` URL names: an empty or `localhost` authority is
/// dropped and percent escapes are decoded.
fn file_url_path(rest: &str) -> Result<String> {
    const SHAPE: &str = "A file URL is written `file:///folder/name`. The path itself works too.";
    let path = match rest.strip_prefix("//") {
        Some(after) => {
            let (host, path) = after.split_at(after.find('/').unwrap_or(after.len()));
            if !(host.is_empty() || host.eq_ignore_ascii_case("localhost")) {
                return Err(invalid(format!(
                    "a file URL cannot name another machine, and this one names {host:?}. Give the path as this machine sees it."
                )));
            }
            path
        }
        None => rest,
    };
    if !path.starts_with('/') {
        return Err(invalid(format!(
            "a file URL holds a path that starts at the root. {SHAPE}"
        )));
    }
    if path.contains(['?', '#']) {
        return Err(invalid(
            "a file URL cannot have `?` or `#` in it. Write %3F or %23 for one in a name, or give the path itself.",
        ));
    }
    let mut decoded = percent_decode(path).ok_or_else(|| {
        invalid(format!(
            "a `%` in a file URL is followed by two hex digits, and the result must be text. {SHAPE}"
        ))
    })?;
    // `/C:/data` is how a URL writes the Windows path `C:/data`.
    let drive = matches!(
        decoded.as_bytes(),
        [b'/', letter, b':', after @ ..]
            if letter.is_ascii_alphabetic() && matches!(after.first(), None | Some(b'/' | b'\\'))
    );
    Ok(if drive { decoded.split_off(1) } else { decoded })
}

/// Replaces each `%XX` with the byte it stands for. `None` if an escape is
/// cut short or not hex, or if the bytes are not UTF-8.
pub(crate) fn percent_decode(text: &str) -> Option<String> {
    let mut out = Vec::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('%') {
        let (plain, escaped) = rest.split_at(at);
        out.extend_from_slice(plain.as_bytes());
        out.extend(hex::decode(escaped.get(1..3)?)?);
        rest = escaped.get(3..)?;
    }
    out.extend_from_slice(rest.as_bytes());
    String::from_utf8(out).ok()
}

/// Reads a web address by itself, as [`Location::parse`] reads one, and
/// gives it as a [`Location::Web`] holds it.
///
/// # Errors
///
/// Returns [`Error::InvalidLocation`] if it is not one.
pub(crate) fn web_address(text: &str) -> Result<String> {
    let read = scheme_of(text).map(|(scheme, rest)| (scheme.to_ascii_lowercase(), rest));
    match read {
        Some((scheme, rest)) if matches!(scheme.as_str(), "http" | "https") => {
            match web(&scheme, rest)? {
                Location::Web(address) => Ok(address),
                _ => Err(invalid(format!("{text:?} is not a web address."))),
            }
        }
        _ => Err(invalid(format!(
            "{text:?} is not a web address: one begins `https://` or `http://`."
        ))),
    }
}

/// Checks the parts of a web address that decide where a request goes: it
/// has a host, an optional numeric port and no credentials. The client that
/// fetches it reads the rest.
fn web(scheme: &str, rest: &str) -> Result<Location> {
    let shape = format!("A web address is written `{scheme}://host/path`.");
    let Some(after) = rest.strip_prefix("//") else {
        return Err(invalid(format!("`{scheme}:` is followed by `//`. {shape}")));
    };
    if let Some(odd) = rest.chars().find(|c| c.is_whitespace() || c.is_control()) {
        return Err(invalid(format!(
            "a web address cannot contain {odd:?}. Write it percent-encoded, as in %20 for a space."
        )));
    }
    let authority = after.split(['/', '?', '#']).next().unwrap_or(after);
    if authority.contains('@') {
        return Err(invalid(
            "mux does not take a user name or password inside a web address. Remove everything up to the `@`.",
        ));
    }
    match host_and_port(authority) {
        Some((host, port))
            if !matches!(host, "" | "[]")
                && port.is_none_or(|port| port.parse::<u16>().is_ok()) =>
        {
            Ok(Location::Web(format!("{scheme}:{rest}")))
        }
        _ => Err(invalid(format!(
            "{authority:?} is not a host, or a host and a port number. {shape}"
        ))),
    }
}

/// Splits `host[:port]`. An IPv6 host is in brackets and has colons of its
/// own.
fn host_and_port(authority: &str) -> Option<(&str, Option<&str>)> {
    let host_len = match authority.strip_prefix('[') {
        Some(v6) => v6.find(']')? + 2,
        None => authority.find(':').unwrap_or(authority.len()),
    };
    let (host, after) = authority.split_at(host_len);
    match after.strip_prefix(':') {
        Some(port) => Some((host, Some(port))),
        None => after.is_empty().then_some((host, None)),
    }
}

fn invalid(reason: impl Into<String>) -> Error {
    Error::InvalidLocation(reason.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::Hash;

    const FILE: Protocol = Protocol {
        scheme: "file",
        reads: true,
        writes: true,
        about: "a path",
    };
    const WEB: [Protocol; 2] = [
        Protocol {
            scheme: "http",
            reads: true,
            writes: false,
            about: "a web address",
        },
        Protocol {
            scheme: "https",
            reads: true,
            writes: false,
            about: "a web address",
        },
    ];
    const SWARM: [Protocol; 2] = [
        Protocol {
            scheme: "mux",
            reads: true,
            writes: false,
            about: "a ticket",
        },
        Protocol {
            scheme: "peer",
            reads: false,
            writes: true,
            about: "this machine",
        },
    ];

    fn every() -> Vec<Protocol> {
        [&[FILE][..], &WEB, &SWARM].concat()
    }

    fn parse(text: &str) -> Result<Location> {
        Location::parse(text, &every())
    }

    fn local(text: &str) -> Location {
        Location::Local(text.to_owned())
    }

    /// The sentence a refused location is answered with.
    fn refusal(text: &str) -> String {
        match parse(text) {
            Err(Error::InvalidLocation(reason)) => reason,
            other => panic!("{text:?} gave {other:?}"),
        }
    }

    #[test]
    fn a_text_without_a_scheme_is_a_path() {
        for path in [
            ".",
            "data",
            "./data",
            "../up/data",
            "/srv/data",
            "dir/a:b",
            "~/data",
            " spaced out ",
            "--help",
            "weights.safetensors",
        ] {
            assert_eq!(parse(path).unwrap(), local(path));
        }
    }

    #[test]
    fn a_windows_drive_is_a_path_not_a_scheme() {
        for path in [
            r"C:\data",
            "C:/data",
            "c:data",
            "Z:",
            r"\\server\share\data",
            r"\\?\C:\data",
        ] {
            assert_eq!(parse(path).unwrap(), local(path));
        }
    }

    #[test]
    fn a_file_url_is_the_path_it_holds() {
        for (url, path) in [
            ("file:///srv/data", "/srv/data"),
            ("file://localhost/srv/data", "/srv/data"),
            ("FILE://LocalHost/srv/data", "/srv/data"),
            ("file:/srv/data", "/srv/data"),
            ("file:///", "/"),
            ("file:///a%20b/%E2%82%AC", "/a b/€"),
            ("file:///100%25", "/100%"),
            ("file:///C:/data", "C:/data"),
            ("file:///c:", "c:"),
            (r"file:///C:\data", r"C:\data"),
            ("file:///C:data", "/C:data"),
            ("file:///CD:/data", "/CD:/data"),
        ] {
            assert_eq!(parse(url).unwrap(), local(path), "{url}");
        }
    }

    #[test]
    fn a_file_url_that_is_not_a_local_path_is_refused() {
        for (url, hint) in [
            ("file://server/share", "another machine"),
            ("file:data", "file:///folder/name"),
            ("file://", "file:///folder/name"),
            ("file:///a?b", "%3F"),
            ("file:///a#b", "%23"),
            ("file:///a%2", "two hex digits"),
            ("file:///a%zz", "two hex digits"),
            ("file:///a%ff", "must be text"),
            ("file:///a%00b", "NUL"),
        ] {
            let reason = refusal(url);
            assert!(reason.contains(hint), "{url}: {reason}");
        }
    }

    #[test]
    fn a_web_address_keeps_its_text() {
        for url in [
            "http://example.org",
            "https://example.org/",
            "https://example.org:8443/big.iso?token=1#part",
            "http://10.0.0.5:8080/a/b",
            "http://[::1]:8080/a",
            "https://[2001:db8::1]/a",
            "https://example.org/a%20b/é",
            "https://example.org?download=1",
        ] {
            assert_eq!(parse(url).unwrap(), Location::Web(url.to_owned()));
        }
        assert_eq!(
            parse("HTTPS://Example.org/A").unwrap(),
            Location::Web("https://Example.org/A".to_owned())
        );
    }

    #[test]
    fn a_web_address_that_names_no_host_is_refused() {
        for (url, hint) in [
            ("https:example.org", "followed by `//`"),
            ("https:/example.org", "followed by `//`"),
            ("https://", "is not a host"),
            ("https:///path", "is not a host"),
            ("https://:443/", "is not a host"),
            ("https://[]/", "is not a host"),
            ("https://[::1/", "is not a host"),
            ("https://[::1]x/", "is not a host"),
            ("https://host:port/", "port number"),
            ("https://host:/", "port number"),
            ("https://host:65536/", "port number"),
            ("https://user:secret@host/", "user name or password"),
            ("https://host/a b", "%20"),
            ("https://host/a\tb", "percent-encoded"),
        ] {
            let reason = refusal(url);
            assert!(reason.contains(hint), "{url}: {reason}");
        }
    }

    #[test]
    fn a_ticket_and_this_machine_are_places() {
        let id = Hash::of(b"content");
        let secret = "5e".repeat(16);
        let peer = "ab".repeat(32);
        let text = format!("mux://{id}?secret={secret}&peer={peer}@10.0.0.5:4433");
        let Location::Ticket(ticket) = parse(&text).unwrap() else {
            panic!("not a ticket");
        };
        assert_eq!(ticket.artifact, id);
        assert_eq!(ticket.peers.len(), 1);
        assert_eq!(
            parse(&text.replacen("mux", "MUX", 1)).unwrap(),
            Location::Ticket(ticket)
        );

        // A message names the ticket and leaves its secret out.
        let named = parse(&text).unwrap().in_a_message();
        assert_eq!(named, format!("mux://{id}"));
        assert_eq!(parse("./a b").unwrap().in_a_message(), "./a b");

        assert_eq!(parse("peer:").unwrap(), Location::Serve);
        assert_eq!(parse("PEER:").unwrap(), Location::Serve);
        assert!(refusal("peer:4433").contains("Write `peer:`"));
    }

    #[test]
    fn a_bad_ticket_says_what_a_ticket_looks_like() {
        let error = parse("mux://not-an-id").unwrap_err();
        assert!(matches!(error, Error::InvalidTicket(_)), "{error}");
        assert!(error.to_string().contains("64 hex characters"));
    }

    #[test]
    fn nothing_and_a_pipe_are_refused() {
        assert!(refusal("").contains("Write a path"));
        assert!(refusal("-").contains("./-"));
    }

    #[test]
    fn a_scheme_nobody_knows_lists_what_this_build_can_use() {
        let reason = refusal("ftp://example.org/file");
        assert!(reason.contains("`ftp:` is not a kind of place mux knows"));
        assert!(reason.contains("file, http, https, mux, peer"));
        // It may have been a file with a colon in its name.
        assert!(refusal("notes:final").contains("write `./notes:final`"));
    }

    #[test]
    fn a_reserved_scheme_is_named_for_what_it_will_be() {
        for (text, place) in [
            ("s3://bucket/key", "cloud storage"),
            ("gs://bucket/key", "cloud storage"),
            ("az://account/key", "cloud storage"),
            ("sftp://host/path", "cloud storage"),
            ("hf://org/model", "model registries"),
            ("oci://registry/image", "container registries"),
            ("magnet:?xt=urn:btih:abc", "torrents"),
        ] {
            let reason = refusal(text);
            assert!(reason.contains("cannot use"), "{text}: {reason}");
            assert!(reason.contains(place), "{text}: {reason}");
        }
    }

    #[test]
    fn a_build_without_a_backend_refuses_its_locations() {
        let only_files = [FILE];
        assert_eq!(Location::parse("data", &only_files).unwrap(), local("data"));
        for text in ["https://example.org/a", "peer:", "mux://abc"] {
            let Err(Error::InvalidLocation(reason)) = Location::parse(text, &only_files) else {
                panic!("{text:?} was accepted");
            };
            assert!(reason.contains("this build of mux cannot use"), "{reason}");
            assert!(reason.contains("It can use: file."), "{reason}");
            // A place that a feature would add is named with its feature.
            let optional = text.starts_with("https");
            assert_eq!(
                reason.contains("without the `web` feature"),
                optional,
                "{reason}"
            );
        }
        // A build with no local storage cannot take a path either.
        assert!(Location::parse("data", &WEB).is_err());
    }

    #[test]
    fn a_protocol_without_a_rule_is_refused() {
        let odd = [Protocol {
            scheme: "s3",
            reads: true,
            writes: true,
            about: "a bucket",
        }];
        let Err(Error::InvalidLocation(reason)) = Location::parse("s3://bucket/key", &odd) else {
            panic!("accepted");
        };
        assert!(reason.contains("no rule"), "{reason}");
    }

    #[test]
    fn printing_gives_text_that_reads_back_as_the_same_place() {
        for text in [
            "data",
            "/srv/data",
            r"C:\data",
            "https://example.org:8443/a?b#c",
            "peer:",
        ] {
            let location = parse(text).unwrap();
            assert_eq!(location.to_string(), text);
            assert_eq!(parse(&location.to_string()).unwrap(), location);
        }
        // A file URL prints as the path it holds.
        assert_eq!(parse("file:///srv/a%20b").unwrap().to_string(), "/srv/a b");
    }

    #[test]
    fn a_path_that_looks_like_something_else_prints_as_a_path() {
        for (path, printed) in [("-", "./-"), ("notes:final", "./notes:final")] {
            let location = local(path);
            assert_eq!(location.to_string(), printed);
            assert_eq!(parse(printed).unwrap(), local(printed));
        }
    }
}
