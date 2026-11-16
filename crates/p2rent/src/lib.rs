//! p2rent: swarm distribution of large artifacts over QUIC.
//!
//! This crate is the one other programs depend on. It gathers the pieces
//! that live in their own crates: the data model, local storage and
//! machine-to-machine transfer.

pub use p2rent_core::artifact;
pub use p2rent_fs as store;
pub use p2rent_peer::{Error, Result, net, swarm};
