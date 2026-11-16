//! Properties of tickets and wire messages, checked on generated inputs.

use model::artifact::{Bitfield, Hash};
use peer::net::proto::{Have, Request, Response, Status};
use peer::net::{NodeId, PeerInfo};
use peer::swarm::{Ticket, TicketPeer};
use proptest::collection::vec;
use proptest::prelude::*;
use std::net::{IpAddr, SocketAddr};

fn hash() -> impl Strategy<Value = Hash> {
    any::<[u8; 32]>().prop_map(Hash::from_bytes)
}

fn node() -> impl Strategy<Value = NodeId> {
    any::<[u8; 32]>().prop_map(NodeId::from_bytes)
}

/// An address as it may be written in a ticket: an IP address or a DNS name,
/// then a port.
fn address() -> impl Strategy<Value = String> {
    let host = prop_oneof![
        any::<[u8; 4]>().prop_map(|b| IpAddr::from(b).to_string()),
        any::<[u8; 16]>().prop_map(|b| format!("[{}]", IpAddr::from(b))),
        "[a-z][a-z0-9-]{0,12}(\\.[a-z][a-z0-9-]{0,8}){0,2}",
    ];
    (host, any::<u16>()).prop_map(|(host, port)| format!("{host}:{port}"))
}

fn ticket() -> impl Strategy<Value = Ticket> {
    let peer = (node(), address()).prop_map(|(node, addr)| TicketPeer { node, addr });
    (hash(), vec(peer, 0..5)).prop_map(|(artifact, peers)| Ticket { artifact, peers })
}

fn bitfield() -> impl Strategy<Value = Bitfield> {
    (0u64..200, vec(any::<bool>(), 0..200)).prop_map(|(len, bits)| {
        let mut field = Bitfield::new(len);
        for (index, set) in (0..len).zip(bits) {
            if set {
                field.set(index);
            }
        }
        field
    })
}

/// An address as peers exchange it: an IP address and a port. IPv6 scope
/// ids and flow labels mean something only on the machine that holds them,
/// and the wire format does not carry them.
fn peer_info() -> impl Strategy<Value = PeerInfo> {
    (node(), any::<IpAddr>(), any::<u16>()).prop_map(|(node, ip, port)| PeerInfo {
        node,
        addr: SocketAddr::new(ip, port),
    })
}

fn request() -> impl Strategy<Value = Request> {
    prop_oneof![
        hash().prop_map(|artifact| Request::Manifest { artifact }),
        hash().prop_map(|artifact| Request::Status { artifact }),
        (hash(), any::<u64>()).prop_map(|(artifact, index)| Request::Chunk { artifact, index }),
    ]
}

fn response() -> impl Strategy<Value = Response> {
    let have = prop_oneof![Just(Have::All), bitfield().prop_map(Have::Some)];
    prop_oneof![
        any::<u64>().prop_map(|len| Response::Manifest { len }),
        (have, vec(peer_info(), 0..6))
            .prop_map(|(have, peers)| Response::Status(Status { have, peers })),
        any::<u32>().prop_map(|len| Response::Chunk { len }),
        Just(Response::NotFound),
    ]
}

proptest! {
    #[test]
    fn a_ticket_survives_printing_and_parsing(ticket in ticket()) {
        let text = ticket.to_string();
        prop_assert_eq!(text.parse::<Ticket>().unwrap(), ticket);
    }

    #[test]
    fn parsing_a_ticket_never_panics(text in ".{0,200}") {
        if let Ok(ticket) = text.parse::<Ticket>() {
            // Anything accepted prints to something that parses to the same
            // ticket: nothing is silently dropped or reinterpreted.
            prop_assert_eq!(ticket.to_string().parse::<Ticket>().unwrap(), ticket);
        }
    }

    #[test]
    fn parsing_a_near_ticket_never_panics(artifact in hash(), tail in "[ -~]{0,120}") {
        let text = format!("mux://{artifact}{tail}");
        if let Ok(ticket) = text.parse::<Ticket>() {
            prop_assert_eq!(ticket.artifact, artifact);
            prop_assert_eq!(ticket.to_string().parse::<Ticket>().unwrap(), ticket);
        }
    }

    #[test]
    fn a_request_survives_encoding(request in request()) {
        let wire = postcard::to_allocvec(&request).unwrap();
        prop_assert_eq!(postcard::from_bytes::<Request>(&wire).unwrap(), request);
    }

    #[test]
    fn a_response_survives_encoding(response in response()) {
        let wire = postcard::to_allocvec(&response).unwrap();
        prop_assert_eq!(postcard::from_bytes::<Response>(&wire).unwrap(), response);
    }

    /// Whatever bytes a peer sends as a status, the set of chunks we take
    /// from it describes exactly the artifact we asked about, or is refused.
    #[test]
    fn a_status_from_a_stranger_is_checked(wire in vec(any::<u8>(), 0..200), chunks in 0u64..300) {
        let _ = postcard::from_bytes::<Request>(&wire);
        if let Ok(Response::Status(status)) = postcard::from_bytes::<Response>(&wire)
            && let Ok(have) = status.have.into_bitfield(chunks)
        {
            prop_assert!(have.is_well_formed(chunks));
            prop_assert!(have.count_ones() <= chunks);
        }
    }

    #[test]
    fn a_have_set_expands_to_the_right_size(have in bitfield(), chunks in 0u64..300) {
        let well_formed = have.is_well_formed(chunks);
        prop_assert_eq!(Have::Some(have).into_bitfield(chunks).is_ok(), well_formed);
        let full = Have::All.into_bitfield(chunks).unwrap();
        prop_assert!(full.is_full());
        prop_assert_eq!(full.len(), chunks);
    }
}
