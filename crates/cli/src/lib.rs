//! mux: swarm distribution of large artifacts over QUIC.
//!
//! This crate is the one other programs depend on. It gathers the pieces
//! that live in their own crates: the data model, the engine, local storage
//! and machine-to-machine transfer.

// Every public item is documented.
#![warn(missing_docs)]

pub use ::engine;
pub use ::fs as store;
pub use model::location::{Location, Protocol};
pub use model::{artifact, place};
pub use peer::{Error, Result, net, swarm};

/// Every kind of place this build can use: the protocols of the backends
/// compiled into it.
pub fn protocols() -> Vec<Protocol> {
    [&store::PROTOCOLS[..], &peer::PROTOCOLS].concat()
}

/// Reads a location as a person types it: a path, a web address or a
/// ticket.
///
/// # Errors
///
/// Returns [`Error::InvalidLocation`] with a sentence that says what to type
/// instead, or [`Error::InvalidTicket`] for a ticket that cannot be read.
pub fn locate(text: &str) -> Result<Location> {
    Ok(Location::parse(text, &protocols())?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locate_reads_the_places_this_build_lists() {
        assert_eq!(locate("./data").unwrap(), Location::Local("./data".into()));
        assert_eq!(locate("peer:").unwrap(), Location::Serve);

        let error = locate("s3://bucket/key").unwrap_err();
        assert!(matches!(error, Error::InvalidLocation(_)), "{error}");
        let listed: Vec<&str> = protocols().iter().map(|p| p.scheme).collect();
        assert!(error.to_string().contains(&listed.join(", ")), "{error}");
    }
}
