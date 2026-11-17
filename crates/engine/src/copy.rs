//! Copying what a source holds into a place.
//!
//! This is the whole of a transfer from a source whose digests are not
//! known, in the order it happens: ask the source what it holds, leave
//! alone what is already there, fetch the rest, and say how far the bytes
//! were checked.

use crate::{Error, Options, Store, supplier, transfer};
use model::artifact::{Artifact, Tree};
use model::place::{Event, Failure, Observer, Sink, Source, SupplierId, Verification};
use std::sync::Arc;

/// This end of a copy, and the source it copies from, as the transfer
/// tells them apart.
const HERE: SupplierId = [0; 32];
const THE_SOURCE: SupplierId = [1; 32];

/// What a copy did.
#[derive(Debug)]
pub struct Copied {
    /// The tree that is now in the place.
    pub tree: Arc<Tree>,
    /// The artifact that tree is. `None` if nothing was read: then no
    /// digest was learned.
    pub artifact: Option<Arc<Artifact>>,
    /// How far the bytes were checked. `None` if every file was there
    /// already and was taken on its size and time.
    pub verification: Option<Verification>,
    /// Bytes fetched from the source.
    pub fetched: u64,
    /// Bytes kept from an earlier copy that was interrupted.
    pub resumed: u64,
}

/// Copies what `source` holds into a place for it, which `open` makes from
/// the tree the source describes. Files are cut into chunks of
/// `chunk_size` to be fetched.
///
/// A file that is already there, with the size and the time the source
/// gives, is left alone and not read. What an earlier copy left unfinished
/// is carried on from, if it was of the same version of the file.
///
/// # Errors
///
/// Returns [`Error::Place`] if the source cannot be described or the place
/// cannot be written, [`Error::Changed`] if the source changes while it is
/// read, and [`Error::Stalled`] if nothing arrives within the stall
/// timeout.
pub async fn copy<P, F>(
    source: Arc<dyn Source>,
    chunk_size: u32,
    open: F,
    options: Options,
    observer: Observer,
) -> Result<Copied, Error>
where
    P: Source + Sink + 'static,
    F: FnOnce(&Arc<Tree>) -> Arc<P>,
{
    let described = source.describe().await?;
    let tree = Tree::new(described.name, chunk_size, described.entries).map_err(|e| {
        Failure::Corrupt(format!(
            "the source describes a tree that cannot be stored: {e}"
        ))
    })?;
    let tree = Arc::new(tree);
    let place = open(&tree);

    if is_current(&tree, &place).await? {
        return Ok(Copied {
            tree,
            artifact: None,
            verification: None,
            fetched: 0,
            resumed: 0,
        });
    }

    let examined = {
        let observer = observer.clone();
        move |bytes| observer(Event::Examined { bytes })
    };
    let store = Store::learn(tree.clone(), place, described.known, examined).await?;
    let held: u64 = (0..tree.chunk_count())
        .filter(|&index| store.has(index))
        .filter_map(|index| tree.locate(index))
        .map(|chunk| u64::from(chunk.len))
        .sum();
    let from = vec![supplier(THE_SOURCE, tree.clone(), source)];
    let summary = transfer(&store, HERE, from, options, observer).await?;
    store.finish().await?;
    Ok(Copied {
        artifact: store.artifact(),
        verification: Some(summary.verification),
        fetched: tree.total_size() - held,
        resumed: held,
        tree,
    })
}

/// Whether every file of the tree is already in the place, as far as size
/// and time can tell. A tree with no file in it is not: there is nothing
/// to go by.
async fn is_current<P: Sink + 'static>(tree: &Arc<Tree>, place: &Arc<P>) -> Result<bool, Failure> {
    let (tree, place) = (tree.clone(), place.clone());
    let looked = tokio::task::spawn_blocking(move || {
        let mut files = tree
            .entries()
            .iter()
            .enumerate()
            .filter(|(_, e)| e.is_file());
        let mut any = false;
        files
            .try_fold(true, |all, (file, _)| {
                any = true;
                Ok(all && place.is_current(file)?)
            })
            .map(|all| all && any)
    });
    looked.await.unwrap_or_else(|stopped| {
        Err(Failure::Fatal(format!(
            "looking at what is already there was cut short: {stopped}"
        )))
    })
}
