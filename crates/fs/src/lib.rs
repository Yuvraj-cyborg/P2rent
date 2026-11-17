//! Local storage: hashing files into artifacts, and reading and writing
//! their chunks on disk.

// Every public item is documented.
#![warn(missing_docs)]
// A library must not panic on bad input. Tests may.
#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]

mod error;
mod folder;
mod index;
mod io;

pub use error::{Error, Result};
pub use folder::Folder;
pub use index::{Indexed, index, measure};

use model::location::Protocol;

/// The kinds of place this crate provides.
pub const PROTOCOLS: [Protocol; 1] = [Protocol {
    scheme: "file",
    reads: true,
    writes: true,
    about: "a file or folder on this machine; a path with no scheme means the same",
}];
