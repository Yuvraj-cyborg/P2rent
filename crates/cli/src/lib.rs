//! mux: swarm distribution of large artifacts over QUIC.
//!
//! This crate is the one other programs depend on. It gathers the pieces
//! that live in their own crates: the data model, the engine, local storage
//! and machine-to-machine transfer.

// Every public item is documented.
#![warn(missing_docs)]

pub use ::engine;
pub use ::fs as store;
#[cfg(feature = "web")]
pub use ::web;
pub use model::location::{Location, Protocol};
pub use model::{artifact, mirror, place};
pub use peer::{Error, Result, net, swarm};

/// Every kind of place this build can use: the protocols of the backends
/// compiled into it.
pub fn protocols() -> Vec<Protocol> {
    let mut all = Vec::from(store::PROTOCOLS);
    #[cfg(feature = "web")]
    all.extend(web::PROTOCOLS);
    all.extend(peer::PROTOCOLS);
    all
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

    /// A web address is a place in a build that has the `web` feature, and
    /// in one that has not, the refusal names the feature.
    #[test]
    fn a_web_address_is_read_by_a_build_that_can_fetch_it() {
        let read = locate("https://example.org/big.iso");
        if cfg!(feature = "web") {
            let address = Location::Web("https://example.org/big.iso".into());
            assert_eq!(read.unwrap(), address);
        } else {
            let refused = read.unwrap_err().to_string();
            assert!(refused.contains("without the `web` feature"), "{refused}");
        }
    }
}
