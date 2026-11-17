//! Runs a transfer: the tasks, locks and timers around the logic in
//! `model`.
//!
//! `model` decides: which chunk goes where, whether a file is whole. This
//! crate carries those decisions out against a place, which takes what
//! `model` must not have: threads, locks and a runtime.

// Every public item is documented.
#![warn(missing_docs)]
// A library must not panic on bad input. Tests may.
#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]

mod claims;
#[cfg(any(test, feature = "conformance"))]
pub mod conformance;
#[cfg(test)]
mod faults;
mod store;
pub mod sync;
mod transfer;

pub use store::{Put, Store};
pub use transfer::{Error, Options, Summary, transfer};
