//! Any source as a supplier of chunks.
//!
//! A source hands over the bytes of a file in order from wherever it is
//! opened. A transfer asks for chunks. This is the one place that turns the
//! first into the second, so that no backend has to: it keeps a reader open
//! after each chunk, and whoever asks for the chunk that follows reads on
//! from it. For a web server that is one request for a long run of chunks
//! and not one request for each.

use crate::sync;
use model::artifact::{Bitfield, Tree};
use model::place::{
    Arrived, Capabilities, Failure, Pending, Reader, Source, Status, Supplier, SupplierId, Supply,
};
use std::sync::{Arc, Mutex};

/// Most readers kept open with nobody reading from them. One more, and the
/// one that has waited longest is closed.
const MAX_IDLE_READERS: usize = 8;

/// A supplier that reads the chunks of `tree` from `source`. `id` tells it
/// from the other suppliers of a transfer.
pub fn supplier(id: SupplierId, tree: Arc<Tree>, source: Arc<dyn Source>) -> Arc<dyn Supplier> {
    Arc::new(OfSource { id, tree, source })
}

#[derive(Debug)]
struct OfSource {
    id: SupplierId,
    tree: Arc<Tree>,
    source: Arc<dyn Source>,
}

impl Supplier for OfSource {
    fn id(&self) -> SupplierId {
        self.id
    }

    /// Nothing is reached for until a chunk is asked for.
    fn open(&self) -> Pending<'_, Arc<dyn Supply>> {
        let opened: Arc<dyn Supply> = Arc::new(Opened {
            tree: self.tree.clone(),
            source: self.source.clone(),
            readers: Mutex::default(),
        });
        Box::pin(std::future::ready(Ok(opened)))
    }
}

/// A source in use, with the readers that are waiting to be read on from.
#[derive(Debug)]
struct Opened {
    tree: Arc<Tree>,
    source: Arc<dyn Source>,
    /// Each with the chunk that comes next from it.
    readers: Mutex<Vec<(u64, Box<dyn Reader>)>>,
}

impl Opened {
    /// The reader that chunk `index` comes next from, if one is open.
    fn waiting_at(&self, index: u64) -> Option<Box<dyn Reader>> {
        let mut readers = sync::lock(&self.readers);
        let position = readers.iter().position(|(next, _)| *next == index)?;
        Some(readers.remove(position).1)
    }

    /// Keeps a reader for whoever asks for chunk `next`.
    fn keep(&self, next: u64, reader: Box<dyn Reader>) {
        let mut readers = sync::lock(&self.readers);
        if readers.len() >= MAX_IDLE_READERS {
            readers.remove(0);
        }
        readers.push((next, reader));
    }
}

impl Supply for Opened {
    /// A source holds all of what it describes, and knows of nobody else.
    fn status(&self) -> Pending<'_, Status> {
        let status = Status {
            held: Bitfield::full(self.tree.chunk_count()),
            others: Vec::new(),
        };
        Box::pin(std::future::ready(Ok(status)))
    }

    fn chunk<'a>(&'a self, index: u64, arrived: &'a Arrived) -> Pending<'a, Vec<u8>> {
        Box::pin(async move {
            let at = self
                .tree
                .locate(index)
                .ok_or_else(|| Failure::Fatal(format!("there is no chunk {index}")))?;
            let mut reader = match self.waiting_at(index) {
                Some(reader) => reader,
                None => self.source.open(at.file, at.offset).await?,
            };
            let data = reader.read(at.len, arrived).await?;
            // The next chunk of the same file follows on from this one.
            let follows = self.tree.locate(index + 1);
            if follows.is_some_and(|next| next.file == at.file) {
                self.keep(index + 1, reader);
            }
            Ok(data)
        })
    }

    fn capabilities(&self) -> Capabilities {
        self.source.capabilities()
    }
}
