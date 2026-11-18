//! Tickets: the text one machine hands another so it can fetch something.

use crate::artifact::Hash;
use crate::error::{Error, Result};
use crate::hex;
use crate::location::{percent_decode, web_address};
use alloc::borrow::ToOwned;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;
use core::str::FromStr;
use serde::{Deserialize, Serialize};

const SCHEME: &str = "mux://";

/// A node's Ed25519 public key. This is the only name a node has.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NodeId([u8; 32]);

impl NodeId {
    /// Wraps a raw Ed25519 public key.
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The raw Ed25519 public key.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// First eight hex digits, for log lines.
    pub fn short(&self) -> String {
        hex::encode(&self.0[..4])
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex::encode(&self.0))
    }
}

impl fmt::Debug for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NodeId({})", self.short())
    }
}

impl FromStr for NodeId {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        hex::decode_32(s)
            .map(Self)
            .ok_or_else(|| invalid("a node id is 64 hex characters"))
    }
}

/// Everything needed to join a swarm: the artifact id and the first peers
/// to ask.
///
/// Written as `mux://<artifact>?peer=<node id>&peer=...`. A peer may be
/// followed by `@` and where to reach it. The artifact id is a content
/// hash, so a ticket also pins exactly what will be downloaded.
///
/// `&url=<web address>` may follow, once for each address: see
/// [`Ticket::origins`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ticket {
    /// The artifact to fetch.
    pub artifact: Hash,
    /// Peers to contact first.
    pub peers: Vec<TicketPeer>,
    /// Web addresses said to hold the same bytes, for an artifact that is
    /// one file: each as a [`Location::Web`](crate::location::Location)
    /// holds it. They are one more place to fetch chunks from, and are
    /// trusted no more than a peer is: every chunk is compared with the
    /// digest the manifest gives for it.
    ///
    /// In a ticket's text `%`, `&`, `#` and `?` of an address are written
    /// as `%25`, `%26`, `%23` and `%3F`, so that the address ends where the
    /// next parameter begins.
    pub origins: Vec<String>,
}

/// A peer named in a ticket.
///
/// Its node id is all that is needed to reach it where nodes can be looked
/// up by id. A ticket may also say where it is, which is tried first: one
/// entry for each place, all with the same node id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketPeer {
    /// The node id the peer must prove.
    pub node: NodeId,
    /// Where to reach it, if the ticket says.
    pub at: Option<Whereabouts>,
}

/// Where a ticket says a peer can be reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Whereabouts {
    /// `host:port`. The host may be a DNS name, resolved when dialling.
    Address(String),
    /// The web address of a relay that passes packets on to it, beginning
    /// `https://` or `http://`.
    Relay(String),
}

impl Whereabouts {
    /// The text as it is written in a ticket.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Address(text) | Self::Relay(text) => text,
        }
    }
}

impl FromStr for Whereabouts {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        if s.contains(['&', '?', '@', '#', ' ']) {
            return Err(invalid(format!("{s:?} is not somewhere a peer can be")));
        }
        let relay = ["https://", "http://"]
            .iter()
            .find_map(|scheme| s.strip_prefix(scheme));
        if let Some(host) = relay {
            if host.is_empty() || host.starts_with('/') {
                return Err(invalid(format!("{s:?} names no relay")));
            }
            return Ok(Self::Relay(s.to_owned()));
        }
        let valid = s
            .rsplit_once(':')
            .is_some_and(|(host, port)| !host.is_empty() && port.parse::<u16>().is_ok());
        if !valid {
            return Err(invalid(format!(
                "{s:?} is neither a host:port address nor the address of a relay"
            )));
        }
        Ok(Self::Address(s.to_owned()))
    }
}

impl FromStr for TicketPeer {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        let (node, at) = match s.split_once('@') {
            Some((node, at)) => (node, Some(at.parse()?)),
            None => (s, None),
        };
        let node = node
            .parse()
            .map_err(|_| invalid("a node id is 64 hex characters"))?;
        Ok(Self { node, at })
    }
}

impl fmt::Display for TicketPeer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.node)?;
        match &self.at {
            Some(at) => write!(f, "@{}", at.as_str()),
            None => Ok(()),
        }
    }
}

impl FromStr for Ticket {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        let rest = s
            .trim()
            .strip_prefix(SCHEME)
            .ok_or_else(|| invalid(format!("a ticket starts with {SCHEME}")))?;
        let (artifact, query) = rest.split_once('?').unwrap_or((rest, ""));
        let artifact = artifact
            .parse()
            .map_err(|_| invalid("the artifact id is 64 hex characters"))?;

        let (mut peers, mut origins) = (Vec::new(), Vec::new());
        for parameter in query.split('&').filter(|p| !p.is_empty()) {
            // Unknown parameters are skipped so tickets can grow new fields.
            if let Some(peer) = parameter.strip_prefix("peer=") {
                peers.push(peer.parse()?);
            } else if let Some(written) = parameter.strip_prefix("url=") {
                origins.push(origin(written)?);
            }
        }
        Ok(Self {
            artifact,
            peers,
            origins,
        })
    }
}

/// Reads a web address as a ticket writes it.
fn origin(written: &str) -> Result<String> {
    let address = percent_decode(written).ok_or_else(|| {
        invalid(format!(
            "in the web address {written:?} a `%` is not followed by two hex digits"
        ))
    })?;
    web_address(&address).map_err(|why| invalid(format!("it names a web address, and {why}")))
}

impl fmt::Display for Ticket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{SCHEME}{}", self.artifact)?;
        let mut separator = '?';
        for peer in &self.peers {
            write!(f, "{separator}peer={peer}")?;
            separator = '&';
        }
        for address in &self.origins {
            write!(f, "{separator}url=")?;
            separator = '&';
            // What would end the address early, and the sign that says so.
            for character in address.chars() {
                match character {
                    '%' => f.write_str("%25")?,
                    '&' => f.write_str("%26")?,
                    '#' => f.write_str("%23")?,
                    '?' => f.write_str("%3F")?,
                    other => fmt::Write::write_char(f, other)?,
                }
            }
        }
        Ok(())
    }
}

fn invalid(reason: impl Into<String>) -> Error {
    Error::InvalidTicket(reason.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(byte: u8) -> NodeId {
        NodeId::from_bytes([byte; 32])
    }

    #[test]
    fn a_node_id_prints_as_hex() {
        let id = node(0xab);
        assert_eq!(id.to_string(), "ab".repeat(32));
        assert_eq!(id.to_string().parse::<NodeId>().unwrap(), id);
        assert_eq!(id.short(), "abababab");
        assert_eq!(format!("{id:?}"), "NodeId(abababab)");
        assert!("abab".parse::<NodeId>().is_err());
    }

    #[test]
    fn round_trips() {
        let at = |node, text: &str| TicketPeer {
            node,
            at: Some(Whereabouts::Address(text.into())),
        };
        let ticket = Ticket {
            artifact: Hash::of(b"artifact"),
            peers: vec![
                at(node(1), "10.0.0.5:4433"),
                at(node(2), "[fd00::1]:4433"),
                at(node(3), "seed.internal:4433"),
                TicketPeer {
                    node: node(4),
                    at: Some(Whereabouts::Relay("https://relay.example./".into())),
                },
                TicketPeer {
                    node: node(5),
                    at: None,
                },
            ],
            origins: vec![
                "https://example.org/big.iso".into(),
                "http://[fd00::1]:8080/a%20b?x=1&y=2#part".into(),
            ],
        };
        let text = ticket.to_string();
        assert!(text.starts_with("mux://"));
        assert_eq!(text.parse::<Ticket>().unwrap(), ticket);
    }

    #[test]
    fn a_web_address_is_written_so_that_it_ends_where_the_next_parameter_begins() {
        let id = Hash::of(b"a");
        let ticket = Ticket {
            artifact: id,
            peers: Vec::new(),
            origins: vec![
                "https://example.org/a%20b?x=1&y=2#part".into(),
                "http://mirror.example/b".into(),
            ],
        };
        // An address that needs nothing written another way is as it is.
        assert_eq!(
            ticket.to_string(),
            format!(
                "mux://{id}?url=https://example.org/a%2520b%3Fx=1%26y=2%23part\
                 &url=http://mirror.example/b"
            )
        );
        // The scheme is kept in small letters, as a location keeps it.
        let read: Ticket = format!("mux://{id}?url=HTTPS://Example.org/")
            .parse()
            .unwrap();
        assert_eq!(read.origins, ["https://Example.org/"]);
    }

    #[test]
    fn a_peer_is_its_node_id_and_perhaps_where_it_is() {
        let id = node(7);
        let read = |at: &str| format!("{id}{at}").parse::<TicketPeer>().unwrap().at;
        assert_eq!(read(""), None);
        assert_eq!(read("@h:1"), Some(Whereabouts::Address("h:1".into())));
        for relay in [
            "https://r.example",
            "http://127.0.0.1:3340",
            "https://r.example./",
        ] {
            assert_eq!(
                read(&format!("@{relay}")),
                Some(Whereabouts::Relay(relay.into()))
            );
        }
        assert_eq!(
            format!("{id}@h:1")
                .parse::<TicketPeer>()
                .unwrap()
                .to_string(),
            format!("{id}@h:1")
        );
        assert_eq!(
            id.to_string().parse::<TicketPeer>().unwrap().to_string(),
            id.to_string()
        );
    }

    #[test]
    fn a_ticket_without_peers_is_just_the_artifact() {
        let ticket = Ticket {
            artifact: Hash::of(b"artifact"),
            peers: Vec::new(),
            origins: Vec::new(),
        };
        assert!(!ticket.to_string().contains('?'));
        assert_eq!(ticket.to_string().parse::<Ticket>().unwrap(), ticket);
    }

    #[test]
    fn ignores_unknown_parameters() {
        let text = format!("mux://{}?future=1&peer={}@h:1", Hash::of(b"a"), node(7));
        assert_eq!(text.parse::<Ticket>().unwrap().peers.len(), 1);
    }

    #[test]
    fn rejects_malformed_tickets() {
        let id = Hash::of(b"a");
        let peer = node(7);
        for text in [
            String::new(),
            format!("{id}"),
            "mux://abc".to_owned(),
            format!("mux://{id}?peer=nope@h:1"),
            format!("mux://{id}?peer=nope"),
            format!("mux://{id}?peer={peer}@"),
            format!("mux://{id}?peer={peer}@host"),
            format!("mux://{id}?peer={peer}@:1"),
            format!("mux://{id}?peer={peer}@h:99999"),
            format!("mux://{id}?peer={peer}@h:1@h:2"),
            format!("mux://{id}?peer={peer}@h:1#x"),
            format!("mux://{id}?peer={peer}@h :1"),
            format!("mux://{id}?peer={peer}@https://"),
            format!("mux://{id}?peer={peer}@https:///path"),
            format!("mux://{id}?peer={peer}@ftp://relay.example"),
            format!("mux://{id}?url="),
            format!("mux://{id}?url=example.org/file"),
            format!("mux://{id}?url=ftp://example.org/file"),
            format!("mux://{id}?url=https://"),
            format!("mux://{id}?url=https://user@example.org/"),
            format!("mux://{id}?url=https://example.org/a b"),
            format!("mux://{id}?url=https://example.org/%zz"),
            format!("mux://{id}?url=https://example.org/%2"),
        ] {
            assert!(text.parse::<Ticket>().is_err(), "{text:?} was accepted");
        }
    }
}
