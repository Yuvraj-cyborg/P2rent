//! p2rent: swarm distribution of large artifacts over QUIC.

// A library must not panic on bad input. Tests may.
#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]

pub mod artifact;
pub mod error;
pub mod net;
pub mod store;
pub mod swarm;

mod hex;
mod sync;

pub use error::{Error, Result};
