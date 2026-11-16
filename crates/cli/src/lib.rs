//! mux: swarm distribution of large artifacts over QUIC.
//!
//! This crate is the one other programs depend on. It gathers the pieces
//! that live in their own crates: the data model, local storage and
//! machine-to-machine transfer.

pub use ::fs as store;
pub use model::artifact;
pub use peer::{Error, Result, net, swarm};
