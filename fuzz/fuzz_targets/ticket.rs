//! Tickets and hashes are pasted in by people and passed between programs.
//! Parsing must not panic, and anything accepted must print back to text
//! that parses to the same value.

#![no_main]

use libfuzzer_sys::fuzz_target;
use model::artifact::Hash;
use model::hex;
use peer::net::NodeId;
use peer::swarm::Ticket;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = core::str::from_utf8(data) else {
        return;
    };

    // Most of what a ticket can hold follows its id and its secret, and
    // random text seldom gets that far by itself.
    let near = format!("mux://{}?secret={}{text}", "ab".repeat(32), "5e".repeat(16));
    for written in [text, near.as_str()] {
        if let Ok(ticket) = written.parse::<Ticket>() {
            let again = ticket
                .to_string()
                .parse::<Ticket>()
                .expect("a printed ticket parses");
            assert_eq!(again, ticket);
        }
    }
    if let Ok(hash) = text.parse::<Hash>() {
        assert_eq!(hash.to_string().parse::<Hash>().ok(), Some(hash));
    }
    if let Ok(node) = text.parse::<NodeId>() {
        assert_eq!(node.to_string().parse::<NodeId>().ok(), Some(node));
    }
    if let Some(bytes) = hex::decode(text) {
        assert_eq!(hex::encode(&bytes), text.to_ascii_lowercase());
    }
});
