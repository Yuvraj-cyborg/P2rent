//! The mux data model: content hashes, manifests and chunk layout.
//!
//! Nothing in this crate touches a disk, a network or a clock, and it does
//! not use `std`. That keeps its logic small enough to prove and lets it run
//! anywhere.

#![cfg_attr(not(test), no_std)]
// Every public item is documented.
#![warn(missing_docs)]
// A library must not panic on bad input. Tests may.
#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]

extern crate alloc;

pub mod artifact;
pub mod assembly;
mod error;
pub mod hex;
pub mod location;
pub mod place;
pub mod retry;
pub mod schedule;
pub mod ticket;

pub use error::{Error, Result};
