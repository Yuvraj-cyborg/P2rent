//! Tickets: the text one machine hands another so it can fetch something.

use crate::access::Secret;
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

/// Everything needed to fetch what a machine offers: the artifact id, the
/// secret that lets its holder ask, and the first peers to ask.
///
/// Written as `mux://<artifact>?secret=<secret>&peer=<node id>&peer=...`. A
/// peer may be followed by `@` and where to reach it. The artifact id is a
/// content hash, so a ticket also pins exactly what will be downloaded.
///
/// `&swarm=1` may follow: see [`Ticket::swarm`]. And `&url=<web address>`,
/// once for each address: see [`Ticket::origins`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ticket {
    /// The artifact to fetch.
    pub artifact: Hash,
    /// What a machine must show it knows before a peer answers it. It is
    /// made new for each offer, so a ticket is good for that offer alone.
    pub secret: Secret,
    /// Whether the machines that fetch this also give it to each other.
    /// Each then serves what it has to every holder of the ticket, and is
    /// told where the others are. Otherwise only the peers of the ticket
    /// serve, and nobody is told of anybody else.
    pub swarm: bool,
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
        let (mut secret, mut swarm) = (None, false);
        for parameter in query.split('&').filter(|p| !p.is_empty()) {
            // Unknown parameters are skipped so tickets can grow new fields.
            if let Some(peer) = parameter.strip_prefix("peer=") {
                peers.push(peer.parse()?);
            } else if let Some(written) = parameter.strip_prefix("url=") {
                origins.push(origin(written)?);
            } else if let Some(written) = parameter.strip_prefix("secret=") {
                let read = written
                    .parse()
                    .map_err(|_| invalid("the secret is 32 hex characters"))?;
                if secret.replace(read).is_some() {
                    return Err(invalid("it has two secrets"));
                }
            } else if let Some(written) = parameter.strip_prefix("swarm=") {
                swarm = match written {
                    "1" => true,
                    "0" => false,
                    _ => return Err(invalid("`swarm=` is followed by 1 or 0")),
                };
            }
        }
        // Tickets had no secret before 0.4, and what made them does not
        // speak to this version.
        let secret = secret.ok_or_else(|| {
            invalid(
                "it has no secret, so an older mux made it, and this one does not talk \
                 to that. Ask for a new ticket.",
            )
        })?;
        Ok(Self {
            artifact,
            secret,
            swarm,
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
        write!(
            f,
            "{SCHEME}{}?secret={}",
            self.artifact,
            self.secret.to_hex()
        )?;
        for peer in &self.peers {
            write!(f, "&peer={peer}")?;
        }
        if self.swarm {
            f.write_str("&swarm=1")?;
        }
        for address in &self.origins {
            f.write_str("&url=")?;
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

    const SECRET: Secret = Secret::from_bytes([0x5e; 16]);

    /// The text of a ticket for `id` with [`SECRET`], and then `rest`.
    fn written(id: Hash, rest: &str) -> String {
        format!("mux://{id}?secret={}{rest}", "5e".repeat(16))
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
            secret: SECRET,
            swarm: true,
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

        let alone = Ticket {
            swarm: false,
            ..ticket
        };
        assert_eq!(alone.to_string().parse::<Ticket>().unwrap(), alone);
    }

    #[test]
    fn a_ticket_says_its_secret_and_whether_it_is_a_swarms() {
        let id = Hash::of(b"a");
        let ticket = Ticket {
            artifact: id,
            secret: SECRET,
            swarm: false,
            peers: vec![TicketPeer {
                node: node(7),
                at: None,
            }],
            origins: Vec::new(),
        };
        let peer = format!("&peer={}", node(7));
        assert_eq!(ticket.to_string(), written(id, &peer));
        let swarm = Ticket {
            swarm: true,
            ..ticket.clone()
        };
        assert_eq!(swarm.to_string(), written(id, &format!("{peer}&swarm=1")));
        // Written out, a ticket that is not a swarm's reads the same.
        let read: Ticket = written(id, &format!("{peer}&swarm=0")).parse().unwrap();
        assert_eq!(read, ticket);
        // The secret is not in what a log line shows of a ticket.
        assert!(!format!("{ticket:?}").contains(&"5e".repeat(16)));
    }

    #[test]
    fn a_ticket_from_before_there_were_secrets_says_to_ask_for_a_new_one() {
        let old = format!("mux://{}?peer={}@h:1", Hash::of(b"a"), node(7));
        let refused = old.parse::<Ticket>().unwrap_err().to_string();
        assert!(refused.contains("Ask for a new ticket"), "{refused}");
    }

    #[test]
    fn a_web_address_is_written_so_that_it_ends_where_the_next_parameter_begins() {
        let id = Hash::of(b"a");
        let ticket = Ticket {
            artifact: id,
            secret: SECRET,
            swarm: false,
            peers: Vec::new(),
            origins: vec![
                "https://example.org/a%20b?x=1&y=2#part".into(),
                "http://mirror.example/b".into(),
            ],
        };
        // An address that needs nothing written another way is as it is.
        assert_eq!(
            ticket.to_string(),
            written(
                id,
                "&url=https://example.org/a%2520b%3Fx=1%26y=2%23part\
                 &url=http://mirror.example/b"
            )
        );
        // The scheme is kept in small letters, as a location keeps it.
        let read: Ticket = written(id, "&url=HTTPS://Example.org/").parse().unwrap();
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
    fn a_ticket_without_peers_is_the_artifact_and_its_secret() {
        let ticket = Ticket {
            artifact: Hash::of(b"artifact"),
            secret: SECRET,
            swarm: false,
            peers: Vec::new(),
            origins: Vec::new(),
        };
        assert_eq!(ticket.to_string(), written(ticket.artifact, ""));
        assert_eq!(ticket.to_string().parse::<Ticket>().unwrap(), ticket);
    }

    #[test]
    fn ignores_unknown_parameters() {
        let peer = format!("&future=1&peer={}@h:1", node(7));
        let text = written(Hash::of(b"a"), &peer);
        assert_eq!(text.parse::<Ticket>().unwrap().peers.len(), 1);
    }

    #[test]
    fn rejects_malformed_tickets() {
        let id = Hash::of(b"a");
        let peer = node(7);
        // Each is wrong in one way alone: with what is wrong taken out,
        // it is read.
        assert!(
            written(id, &format!("&peer={peer}@h:1"))
                .parse::<Ticket>()
                .is_ok()
        );
        let secret = "5e".repeat(16);
        for text in [
            String::new(),
            format!("{id}"),
            "mux://abc".to_owned(),
            format!("mux://{id}"),
            format!("mux://{id}?peer={peer}@h:1"),
            format!("mux://{id}?secret="),
            format!("mux://{id}?secret=5e5e"),
            format!("mux://{id}?secret={secret}5e"),
            format!("mux://{id}?secret={}", "zz".repeat(16)),
            written(id, &format!("&secret={secret}")),
            written(id, "&swarm="),
            written(id, "&swarm=yes"),
            written(id, "&peer=nope@h:1"),
            written(id, "&peer=nope"),
            written(id, &format!("&peer={peer}@")),
            written(id, &format!("&peer={peer}@host")),
            written(id, &format!("&peer={peer}@:1")),
            written(id, &format!("&peer={peer}@h:99999")),
            written(id, &format!("&peer={peer}@h:1@h:2")),
            written(id, &format!("&peer={peer}@h:1#x")),
            written(id, &format!("&peer={peer}@h :1")),
            written(id, &format!("&peer={peer}@https://")),
            written(id, &format!("&peer={peer}@https:///path")),
            written(id, &format!("&peer={peer}@ftp://relay.example")),
            written(id, "&url="),
            written(id, "&url=example.org/file"),
            written(id, "&url=ftp://example.org/file"),
            written(id, "&url=https://"),
            written(id, "&url=https://user@example.org/"),
            written(id, "&url=https://example.org/a b"),
            written(id, "&url=https://example.org/%zz"),
            written(id, "&url=https://example.org/%2"),
        ] {
            assert!(text.parse::<Ticket>().is_err(), "{text:?} was accepted");
        }
    }
}
