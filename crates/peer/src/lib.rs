//! Machine-to-machine transfer: node identity, authenticated QUIC
//! connections, the wire protocol and the swarm built on them.

// Every public item is documented.
#![warn(missing_docs)]
// A library must not panic on bad input. Tests may.
#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]

mod error;
pub mod net;
pub mod swarm;
mod sync;

pub use error::{Error, Result};

use model::location::Protocol;

/// The kinds of place this crate provides.
pub const PROTOCOLS: [Protocol; 2] = [
    Protocol {
        scheme: "mux",
        reads: true,
        writes: false,
        about: "content held by other machines, named by a ticket",
    },
    Protocol {
        scheme: "peer",
        reads: false,
        writes: true,
        about: "this machine, serving what it is given to other machines",
    },
];
