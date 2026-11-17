//! Web addresses: HTTP and HTTPS as a place to read from.
//!
//! This crate knows how to ask a web server for a file. It does no
//! scheduling, retrying or checking of its own: an [`Origin`] is a
//! [`model::place::Source`], and the engine does the rest.

// Every public item is documented.
#![warn(missing_docs)]
// A library must not panic on bad input. Tests may.
#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]

pub mod headers;
mod origin;

pub use origin::{Options, Origin};

use model::location::Protocol;

/// The kinds of place this crate provides.
pub const PROTOCOLS: [Protocol; 2] = [
    Protocol {
        scheme: "http",
        reads: true,
        writes: false,
        about: "a file at a web address, sent in the clear",
    },
    Protocol {
        scheme: "https",
        reads: true,
        writes: false,
        about: "a file at a web address",
    },
];
