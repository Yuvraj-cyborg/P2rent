//! Properties of the wire messages, checked on generated inputs.

use model::artifact::{Bitfield, Hash};
use peer::net::proto::{Have, Request, Response, Status};
use peer::net::{NodeId, PeerInfo};
use proptest::collection::vec;
use proptest::prelude::*;
use std::net::{IpAddr, SocketAddr};

fn hash() -> impl Strategy<Value = Hash> {
    any::<[u8; 32]>().prop_map(Hash::from_bytes)
}

fn node() -> impl Strategy<Value = NodeId> {
    any::<[u8; 32]>().prop_map(NodeId::from_bytes)
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

/// A peer as peers tell each other of it: its id, addresses made of an IP
/// address and a port, and perhaps a relay. IPv6 scope ids and flow labels
/// mean something only on the machine that holds them, and the wire format
/// does not carry them.
fn peer_info() -> impl Strategy<Value = PeerInfo> {
    let addr = (any::<IpAddr>(), any::<u16>()).prop_map(|(ip, port)| SocketAddr::new(ip, port));
    let relay = proptest::option::of("https://[a-z]{1,12}\\.example/");
    (node(), vec(addr, 0..4), relay).prop_map(|(node, addrs, relay)| PeerInfo {
        node,
        addrs,
        relay,
    })
}

fn request() -> impl Strategy<Value = Request> {
    prop_oneof![
        hash().prop_map(|artifact| Request::Manifest { artifact }),
        hash().prop_map(|content| Request::Hints { content }),
        hash().prop_map(|content| Request::Status { content }),
        (hash(), any::<u64>()).prop_map(|(content, index)| Request::Chunk { content, index }),
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
        Just(Response::Hints),
    ]
}

proptest! {
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
