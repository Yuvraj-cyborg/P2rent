//! The web addresses a ticket names, as places to fetch its chunks from.
//!
//! A ticket for one file may say where on the web the same bytes are: the
//! address the first machine fetched them from. A machine that holds the
//! ticket then reads from that address and from the machines that have the
//! file in one transfer, and compares every chunk with the digest the
//! manifest gives for it, wherever it came from.

use crate::artifact::{Artifact, Hash};
use crate::place::{Supplier, SupplierId};
use crate::swarm::{Found, Ticket};
use std::sync::Arc;

/// A web address of a ticket, and whether it is used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    /// The address.
    pub address: String,
    /// What tells it from the other suppliers of a transfer: the digest of
    /// the address.
    pub id: SupplierId,
    /// Why nothing is fetched from it, if nothing is.
    pub unused: Option<String>,
}

/// Adds the web addresses `ticket` names to what `found` is fetched from,
/// and says of each whether it was added.
///
/// Each is asked what it holds, and none of the file. One that cannot be
/// reached, or holds a file of another size, is left out and says why: the
/// transfer then has the machines of the ticket to fetch from, as it would
/// with no address.
pub async fn add(found: &mut Found, ticket: &Ticket) -> Vec<Origin> {
    let mut origins = Vec::with_capacity(ticket.origins.len());
    for address in &ticket.origins {
        let id = *Hash::of(address.as_bytes()).as_bytes();
        let unused = match supplier(address, id, &found.artifact).await {
            Ok(supplier) => {
                found.also(supplier);
                None
            }
            Err(why) => Some(why),
        };
        origins.push(Origin {
            address: address.clone(),
            id,
            unused,
        });
    }
    origins
}

/// The file at `address` as a supplier of `artifact`, if it can be one.
#[cfg(feature = "web")]
async fn supplier(
    address: &str,
    id: SupplierId,
    artifact: &Arc<Artifact>,
) -> Result<Arc<dyn Supplier>, String> {
    use crate::artifact::EntryKind;
    use crate::place::Source;

    if !artifact.is_file() {
        return Err("the ticket names a folder, and a web address holds one file".into());
    }
    let origin = crate::web::Origin::find(address, &crate::web::Options::default())
        .await
        .map_err(|failure| failure.to_string())?;
    let described = origin.describe().await.map_err(|e| e.to_string())?;
    let holds = match described.entries.as_slice() {
        [(entry, _)] => match entry.kind {
            EntryKind::File { size } => Some(size),
            _ => None,
        },
        _ => None,
    };
    if holds != Some(artifact.total_size()) {
        return Err(format!(
            "it holds {} bytes, and the ticket names a file of {}",
            holds.unwrap_or(0),
            artifact.total_size()
        ));
    }
    let tree = artifact.tree().clone();
    Ok(crate::engine::supplier(id, tree, Arc::new(origin)))
}

#[cfg(not(feature = "web"))]
async fn supplier(_: &str, _: SupplierId, _: &Arc<Artifact>) -> Result<Arc<dyn Supplier>, String> {
    Err("this build of mux was made without the `web` feature".into())
}
