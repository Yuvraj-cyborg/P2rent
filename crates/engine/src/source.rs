//! Any source as a supplier of chunks.
//!
//! A source hands over bytes of a file in order from wherever it is
//! opened. A transfer asks for chunks. This is the one place that turns the
//! first into the second, so that no backend has to: when a chunk is asked
//! for, a reader is opened for it and for those that are to follow it, it
//! is kept open after each chunk, and whoever asks for the chunk that
//! follows reads on from it. For a web server that is one request for a
//! run of chunks and not one request for each, and a request that ends
//! where the run does.

use crate::sync;
use model::artifact::{Bitfield, ChunkLocation, Tree};
use model::place::{
    Arrived, Capabilities, Failure, Pending, Reader, Source, Status, Supplier, SupplierId, Supply,
};
use std::sync::{Arc, Mutex};

/// Most readers kept open with nobody reading from them. One more, and the
/// one that has waited longest is closed. A reader waits here while the
/// chunk that was read from it is being stored, so this is more than the
/// streams a transfer is likely to be given.
const MAX_IDLE_READERS: usize = 64;

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
    readers: Mutex<Vec<Waiting>>,
}

/// A reader that has more to give.
#[derive(Debug)]
struct Waiting {
    /// The chunk that comes next from it.
    next: u64,
    /// How many bytes it was opened for and has not yet given.
    left: u64,
    reader: Box<dyn Reader>,
}

impl Opened {
    /// The reader that chunk `index` comes next from, if one is open.
    fn waiting_at(&self, index: u64) -> Option<Waiting> {
        let mut readers = sync::lock(&self.readers);
        let position = readers.iter().position(|waiting| waiting.next == index)?;
        Some(readers.remove(position))
    }

    /// Keeps a reader for whoever asks for the chunk that comes next from
    /// it.
    fn keep(&self, waiting: Waiting) {
        let mut readers = sync::lock(&self.readers);
        if readers.len() >= MAX_IDLE_READERS {
            readers.remove(0);
        }
        readers.push(waiting);
    }

    /// How many bytes there are from the start of chunk `index`, which is
    /// at `at`, to the end of the last of the `following` chunks after it
    /// that is in the same file.
    fn reach(&self, index: u64, at: ChunkLocation, following: u64) -> u64 {
        // No further than the last chunk of the file, so the chunk the
        // reach ends with is one of the file's.
        let last_of_file = self.tree.file_chunks(at.file).end.saturating_sub(1);
        let last = index.saturating_add(following).min(last_of_file);
        self.tree.locate(last).map_or(u64::from(at.len), |last| {
            (last.offset + u64::from(last.len)).saturating_sub(at.offset)
        })
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

    fn chunk<'a>(
        &'a self,
        index: u64,
        following: u64,
        arrived: &'a Arrived,
    ) -> Pending<'a, Vec<u8>> {
        Box::pin(async move {
            let at = self
                .tree
                .locate(index)
                .ok_or_else(|| Failure::Fatal(format!("there is no chunk {index}")))?;
            let mut open = if let Some(waiting) = self.waiting_at(index) {
                waiting
            } else {
                let left = self.reach(index, at, following);
                Waiting {
                    next: index,
                    left,
                    reader: self.source.open(at.file, at.offset, left).await?,
                }
            };
            let data = open.reader.read(at.len, arrived).await?;
            // The next chunk follows on from this one, if the reader was
            // opened for it too.
            open.next = index + 1;
            open.left = open.left.saturating_sub(u64::from(at.len));
            if open.left > 0 {
                self.keep(open);
            }
            Ok(data)
        })
    }

    fn leave(&self, index: u64) {
        drop(self.waiting_at(index));
    }

    fn capabilities(&self) -> Capabilities {
        self.source.capabilities()
    }
}
