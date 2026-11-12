use crate::artifact::Hash;
use crate::error::{Error, Result};
use crate::net::NodeId;
use std::fmt;
use std::str::FromStr;

const SCHEME: &str = "p2rent://";

/// Everything needed to join a swarm: the artifact id and where to find the
/// first peers.
///
/// Written as `p2rent://<artifact>?peer=<node id>@<host:port>&peer=...`. The
/// artifact id is a content hash, so a ticket also pins exactly what will be
/// downloaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ticket {
    pub artifact: Hash,
    pub peers: Vec<TicketPeer>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketPeer {
    pub node: NodeId,
    /// `host:port`. The host may be a DNS name, resolved when dialling.
    pub addr: String,
}

impl FromStr for TicketPeer {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        let (node, addr) = s
            .split_once('@')
            .ok_or_else(|| invalid("a peer is written <node id>@<host:port>"))?;
        let node = node
            .parse()
            .map_err(|_| invalid("a node id is 64 hex characters"))?;
        let valid = addr
            .rsplit_once(':')
            .is_some_and(|(host, port)| !host.is_empty() && port.parse::<u16>().is_ok());
        if !valid || addr.contains(['&', '?', '@', ' ']) {
            return Err(invalid(format!("{addr:?} is not a host:port address")));
        }
        Ok(Self {
            node,
            addr: addr.to_owned(),
        })
    }
}

impl fmt::Display for TicketPeer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.node, self.addr)
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

        let mut peers = Vec::new();
        for parameter in query.split('&').filter(|p| !p.is_empty()) {
            // Unknown parameters are skipped so tickets can grow new fields.
            if let Some(peer) = parameter.strip_prefix("peer=") {
                peers.push(peer.parse()?);
            }
        }
        Ok(Self { artifact, peers })
    }
}

impl fmt::Display for Ticket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{SCHEME}{}", self.artifact)?;
        for (i, peer) in self.peers.iter().enumerate() {
            let separator = if i == 0 { '?' } else { '&' };
            write!(f, "{separator}peer={peer}")?;
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
    fn round_trips() {
        let ticket = Ticket {
            artifact: Hash::of(b"artifact"),
            peers: vec![
                TicketPeer {
                    node: node(1),
                    addr: "10.0.0.5:4433".into(),
                },
                TicketPeer {
                    node: node(2),
                    addr: "[fd00::1]:4433".into(),
                },
                TicketPeer {
                    node: node(3),
                    addr: "seed.internal:4433".into(),
                },
            ],
        };
        let text = ticket.to_string();
        assert!(text.starts_with("p2rent://"));
        assert_eq!(text.parse::<Ticket>().unwrap(), ticket);
    }

    #[test]
    fn a_ticket_without_peers_is_just_the_artifact() {
        let ticket = Ticket {
            artifact: Hash::of(b"artifact"),
            peers: Vec::new(),
        };
        assert!(!ticket.to_string().contains('?'));
        assert_eq!(ticket.to_string().parse::<Ticket>().unwrap(), ticket);
    }

    #[test]
    fn ignores_unknown_parameters() {
        let text = format!("p2rent://{}?future=1&peer={}@h:1", Hash::of(b"a"), node(7));
        assert_eq!(text.parse::<Ticket>().unwrap().peers.len(), 1);
    }

    #[test]
    fn rejects_malformed_tickets() {
        let id = Hash::of(b"a");
        let peer = node(7);
        for text in [
            String::new(),
            format!("{id}"),
            "p2rent://abc".to_owned(),
            format!("p2rent://{id}?peer=nope@h:1"),
            format!("p2rent://{id}?peer={peer}"),
            format!("p2rent://{id}?peer={peer}@host"),
            format!("p2rent://{id}?peer={peer}@:1"),
            format!("p2rent://{id}?peer={peer}@h:99999"),
        ] {
            assert!(text.parse::<Ticket>().is_err(), "{text:?} was accepted");
        }
    }
}
