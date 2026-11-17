//! What every place must do, as checks that any implementation can run.
//!
//! A backend's tests call [`check`] with a way to open its place. The
//! checks drive the place the way the engine does, through a [`Store`], and
//! say what is wrong with the first thing that is.
//!
//! [`Memory`] is the simplest place that passes: a tree held in memory. It
//! is here to try the checks on, and for tests that want a place without a
//! disk.

use crate::{Put, Store};
use model::artifact::{
    Artifact, Bitfield, Entry, EntryKind, EntryMetadata, Hash, MIN_CHUNK_SIZE, Manifest,
};
use model::place::{Capabilities, Failure, Pending, Sink, Source};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

/// A tree held in memory. Clones share the bytes, so a clone opened again
/// with [`Sink::begin`] sees what an earlier transfer left, as a program
/// started again would.
#[derive(Debug, Clone)]
pub struct Memory {
    artifact: Arc<Artifact>,
    files: Arc<Mutex<Vec<Slot>>>,
    finished: Arc<AtomicBool>,
}

/// One entry's bytes: the version with its final name, and the one being
/// written.
#[derive(Debug, Default)]
struct Slot {
    committed: Option<Vec<u8>>,
    pending: Option<Vec<u8>>,
}

impl Memory {
    /// An empty place for `artifact`.
    pub fn new(artifact: Arc<Artifact>) -> Self {
        let files = artifact.entries().iter().map(|_| Slot::default()).collect();
        Self {
            artifact,
            files: Arc::new(Mutex::new(files)),
            finished: Arc::default(),
        }
    }

    /// Whether the tree has been finished.
    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Relaxed)
    }

    /// The bytes `file` has under its final name, if it has been committed.
    pub fn committed(&self, file: usize) -> Option<Vec<u8>> {
        self.with(|files| files.get(file).and_then(|slot| slot.committed.clone()))
    }

    fn with<T>(&self, work: impl FnOnce(&mut Vec<Slot>) -> T) -> T {
        work(&mut self.files.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// Runs `work` on the bytes of one entry.
    fn slot<T>(
        &self,
        file: usize,
        work: impl FnOnce(&mut Slot) -> Result<T, Failure>,
    ) -> Result<T, Failure> {
        self.with(|files| match files.get_mut(file) {
            Some(slot) => work(slot),
            None => Err(Failure::Absent(format!("there is no file {file}"))),
        })
    }

    /// The chunks of `file` that are right in `bytes`.
    fn right(&self, file: usize, bytes: &[u8]) -> Vec<u64> {
        self.artifact
            .file_chunks(file)
            .filter(|&index| {
                self.artifact.locate(index).is_some_and(|at| {
                    let start = usize::try_from(at.offset).unwrap_or(usize::MAX);
                    let chunk = start
                        .checked_add(at.len as usize)
                        .and_then(|end| bytes.get(start..end));
                    chunk.map(Hash::of) == self.artifact.chunk_hash(index)
                })
            })
            .collect()
    }

    fn open(&self, examined: &(dyn Fn(u64) + Sync)) -> Bitfield {
        let mut held = Bitfield::new(self.artifact.chunk_count());
        for (file, entry) in self.artifact.entries().iter().enumerate() {
            let EntryKind::File { size } = entry.kind else {
                continue;
            };
            examined(size);
            let size = usize::try_from(size).unwrap_or(usize::MAX);
            let chunks = self.artifact.file_chunks(file);
            self.with(|files| {
                let slot = &mut files[file];
                let old = slot.committed.take().unwrap_or_default();
                let reusable = self.right(file, &old);
                let whole = old.len() == size && reusable.len() as u64 == chunks.end - chunks.start;
                let kept = if whole {
                    // Whole and right, which a file with no bytes always is.
                    slot.pending = None;
                    slot.committed = Some(old);
                    reusable
                } else {
                    // Keep what is right in either version, in the new one.
                    let mut pending = slot.pending.take().unwrap_or_default();
                    pending.resize(size, 0);
                    for index in reusable {
                        if let Some(at) = self.artifact.locate(index) {
                            let start = usize::try_from(at.offset).unwrap_or(usize::MAX);
                            let end = start + at.len as usize;
                            pending[start..end].copy_from_slice(&old[start..end]);
                        }
                    }
                    let right = self.right(file, &pending);
                    slot.pending = Some(pending);
                    slot.committed = (!old.is_empty()).then_some(old);
                    right
                };
                for index in kept {
                    held.set(index);
                }
            });
        }
        held
    }
}

fn ready<'a, T: Send + 'a>(result: Result<T, Failure>) -> Pending<'a, T> {
    Box::pin(std::future::ready(result))
}

impl Source for Memory {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            random_access: true,
        }
    }

    fn read(&self, file: usize, offset: u64, len: u32) -> Pending<'_, Vec<u8>> {
        ready(self.slot(file, |slot| {
            // What is being written replaces what is committed, as a part
            // file does, so that is where a chunk just stored is found.
            let bytes = slot.pending.as_ref().or(slot.committed.as_ref());
            let start = usize::try_from(offset).unwrap_or(usize::MAX);
            start
                .checked_add(len as usize)
                .and_then(|end| bytes?.get(start..end))
                .map(<[u8]>::to_vec)
                .ok_or_else(|| Failure::Absent(format!("file {file} does not reach that far")))
        }))
    }
}

impl Sink for Memory {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            random_access: true,
        }
    }

    fn begin(&self, examined: &(dyn Fn(u64) + Sync)) -> Result<Bitfield, Failure> {
        Ok(self.open(examined))
    }

    fn write(&self, file: usize, offset: u64, data: &[u8]) -> Result<(), Failure> {
        self.slot(file, |slot| {
            let start = usize::try_from(offset).unwrap_or(usize::MAX);
            let room = slot
                .pending
                .as_mut()
                .and_then(|pending| pending.get_mut(start..start.checked_add(data.len())?))
                .ok_or_else(|| Failure::Fatal(format!("file {file} has no room for that")))?;
            room.copy_from_slice(data);
            Ok(())
        })
    }

    fn commit(&self, file: usize) -> Result<(), Failure> {
        self.slot(file, |slot| {
            let nothing = || Failure::Fatal(format!("file {file} has nothing to commit"));
            slot.committed = Some(slot.pending.take().ok_or_else(nothing)?);
            Ok(())
        })
    }

    fn abort(&self, file: usize) -> Result<(), Failure> {
        self.slot(file, |slot| {
            slot.pending = None;
            Ok(())
        })
    }

    fn finish(&self) -> Result<(), Failure> {
        self.finished.store(true, Ordering::Relaxed);
        Ok(())
    }
}

/// A small tree and the bytes of each of its files: a file that ends part
/// of the way through a chunk, an empty one, one that is a whole number of
/// chunks, and one of a single byte, some of them inside a directory.
#[derive(Debug)]
pub struct Sample {
    /// The tree.
    pub artifact: Arc<Artifact>,
    /// The bytes of each entry. Empty for a directory.
    pub files: Vec<Vec<u8>>,
}

/// The entries of the sample that the checks name.
const FIRST_FILE: usize = 1;
const SECOND_FILE: usize = 4;

impl Sample {
    /// Builds the tree. The bytes are the same every time.
    ///
    /// # Errors
    ///
    /// Returns an error if the tree is not a valid artifact, which would be
    /// a mistake in this function.
    pub fn new() -> Result<Self, model::Error> {
        let chunk = MIN_CHUNK_SIZE as usize;
        let tree: [(&str, Option<usize>); 6] = [
            ("", None),
            ("a.bin", Some(3 * chunk + 5)),
            ("empty", Some(0)),
            ("sub", None),
            ("sub/b.bin", Some(2 * chunk)),
            ("sub/c", Some(1)),
        ];
        // Bytes that do not repeat, so that no two chunks are the same.
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        let mut next_byte = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[0]
        };
        let files: Vec<Vec<u8>> = tree
            .iter()
            .map(|(_, size)| (0..size.unwrap_or(0)).map(|_| next_byte()).collect())
            .collect();
        let entries = tree
            .iter()
            .map(|&(path, size)| {
                let (kind, mode) = match size {
                    Some(size) => (EntryKind::File { size: size as u64 }, 0o644),
                    None => (EntryKind::Directory, 0o755),
                };
                let entry = Entry {
                    path: path.to_owned(),
                    kind,
                };
                let metadata = EntryMetadata {
                    mode,
                    modified: None,
                };
                (entry, metadata)
            })
            .collect();
        let digests = files
            .iter()
            .flat_map(|bytes| bytes.chunks(chunk).map(Hash::of))
            .collect();
        let manifest = Manifest::new("sample", MIN_CHUNK_SIZE, entries, digests);
        Ok(Self {
            artifact: Arc::new(Artifact::new(manifest)?),
            files,
        })
    }

    /// The bytes of chunk `index`. Empty if the tree has no such chunk.
    pub fn chunk(&self, index: u64) -> Vec<u8> {
        self.artifact
            .locate(index)
            .and_then(|at| {
                let start = usize::try_from(at.offset).ok()?;
                self.files.get(at.file)?.get(start..start + at.len as usize)
            })
            .map(<[u8]>::to_vec)
            .unwrap_or_default()
    }

    fn chunks(&self) -> u64 {
        self.artifact.chunk_count()
    }
}

/// What a place got wrong, in a sentence.
pub type Wrong = String;

/// One run of the checks: how to open a place, and the tree to fill it
/// with.
struct Run<'a, F> {
    open: &'a F,
    sample: Sample,
}

/// Runs every check on places opened by `open`.
///
/// `open` is given the tree and a name. Each name is a place of its own,
/// empty at first. Called again with a name it has seen, `open` must give
/// the same stored bytes, as a program started again would find them.
///
/// # Errors
///
/// Returns what is wrong with the first check the place fails.
pub async fn check<P, F>(open: F) -> Result<(), Wrong>
where
    P: Source + Sink + 'static,
    F: Fn(&Arc<Artifact>, &str) -> Arc<P>,
{
    let sample = Sample::new().map_err(|error| error.to_string())?;
    let run = Run {
        open: &open,
        sample,
    };
    run.a_new_place_holds_nothing().await?;
    run.chunks_arrive_intact_in_any_order().await?;
    run.chunks_written_together_arrive_intact().await?;
    run.what_a_place_says_it_holds_is_right().await?;
    run.an_aborted_file_is_forgotten().await?;
    run.wrong_bytes_are_refused().await?;
    run.a_read_gives_the_whole_range_or_fails().await
}

impl<P, F> Run<'_, F>
where
    P: Source + Sink + 'static,
    F: Fn(&Arc<Artifact>, &str) -> Arc<P>,
{
    async fn a_new_place_holds_nothing(&self) -> Result<(), Wrong> {
        let store = self.resume("new").await?;
        ensure(store.held().count_ones() == 0, "a new place holds chunks")?;
        ensure(!store.is_complete(), "a new place is complete")
    }

    async fn chunks_arrive_intact_in_any_order(&self) -> Result<(), Wrong> {
        let mut order: Vec<u64> = (0..self.sample.chunks()).collect();
        let place = (self.open)(&self.sample.artifact, "probe");
        if Sink::capabilities(&*place).random_access {
            order.reverse();
            order.rotate_left(2);
        }
        let store = self.resume("any-order").await?;
        for index in order {
            ensure(!store.has(index), "a chunk is held before it is sent")?;
            self.put(&store, index).await?;
            ensure(store.has(index), "a chunk is not held once written")?;
            // Every chunk held can be read back at once: a node serves what
            // it has while it fetches the rest.
            self.same(&store, index).await?;
        }
        ensure(store.is_complete(), "every chunk is in and it is not whole")?;
        store.finish().await.map_err(|e| wrong("finishing", &e))
    }

    /// Also: a chunk sent twice changes nothing, and a tree that was
    /// finished is found whole when its place is opened again.
    async fn chunks_written_together_arrive_intact(&self) -> Result<(), Wrong> {
        let store = self.resume("together").await?;
        let writes: Vec<_> = (0..self.sample.chunks())
            .map(|index| {
                let (store, data) = (store.clone(), self.sample.chunk(index));
                tokio::spawn(async move { store.write_chunk(index, data).await })
            })
            .collect();
        for write in writes {
            let kept = write.await.map_err(|e| format!("a write stopped: {e}"))?;
            let stored = matches!(kept, Ok(Put::Stored | Put::Completed(_)));
            ensure(stored, "a chunk written with others was lost")?;
        }
        ensure(store.is_complete(), "writes at one moment left a gap")?;
        self.put(&store, 0).await?;
        store.finish().await.map_err(|e| wrong("finishing", &e))?;

        let again = self.resume("together").await?;
        ensure(again.is_complete(), "a finished tree is not found again")?;
        for index in 0..self.sample.chunks() {
            self.same(&again, index).await?;
        }
        Ok(())
    }

    /// A chunk a place reports is right when read, and a file that was
    /// committed is still held when the place is opened again.
    async fn what_a_place_says_it_holds_is_right(&self) -> Result<(), Wrong> {
        let committed = self.committed_chunks();
        let store = self.resume("part").await?;
        for index in committed.clone() {
            self.put(&store, index).await?;
        }
        // And one chunk of a file that stays unfinished.
        self.put(&store, self.unfinished_chunk()).await?;
        drop(store);

        let again = self.resume("part").await?;
        for index in committed {
            ensure(again.has(index), "a chunk of a committed file is lost")?;
        }
        for index in (0..self.sample.chunks()).filter(|&index| again.has(index)) {
            self.same(&again, index).await?;
        }
        Ok(())
    }

    /// Follows the check above, on the same place.
    async fn an_aborted_file_is_forgotten(&self) -> Result<(), Wrong> {
        let place = (self.open)(&self.sample.artifact, "part");
        for _ in 0..2 {
            // The second time there is nothing to throw away.
            let aborted = place.abort(SECOND_FILE);
            aborted.map_err(|e| wrong("aborting a file", &e))?;
        }
        let again = self.resume("part").await?;
        let forgotten = !again.has(self.unfinished_chunk());
        ensure(forgotten, "an aborted file kept a chunk")?;
        for index in self.committed_chunks() {
            ensure(again.has(index), "an abort touched a committed file")?;
        }
        Ok(())
    }

    async fn wrong_bytes_are_refused(&self) -> Result<(), Wrong> {
        let store = self.resume("wrong").await?;
        let mut flipped = self.sample.chunk(0);
        flipped[7] ^= 1;
        let mut short = self.sample.chunk(0);
        short.pop();
        for bytes in [flipped, short, self.sample.chunk(1), Vec::new()] {
            let kept = store.write_chunk(0, bytes).await;
            let refused = kept == Ok(Put::Refused);
            ensure(refused, "bytes that are not the chunk were taken")?;
        }
        ensure(!store.has(0), "wrong bytes were kept")?;
        let beyond = store.write_chunk(self.sample.chunks(), Vec::new()).await;
        ensure(beyond.is_err(), "a chunk past the end was taken")?;
        let missing = store.read_chunk(0).await;
        ensure(missing.is_err(), "a chunk that is not held was read")
    }

    /// Follows the check that finished the place named "together".
    async fn a_read_gives_the_whole_range_or_fails(&self) -> Result<(), Wrong> {
        let place = (self.open)(&self.sample.artifact, "together");
        let opened = place.begin(&|_| {});
        opened.map_err(|e| wrong("opening a whole tree", &e))?;
        for (file, bytes) in self.sample.files.iter().enumerate() {
            let len = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
            if len == 0 {
                continue;
            }
            let all = place.read(file, 0, len).await;
            ensure(all.as_deref() == Ok(&bytes[..]), "a whole file reads wrong")?;
            let middle = len / 2;
            let tail = place.read(file, u64::from(middle), len - middle).await;
            let expected = &bytes[middle as usize..];
            ensure(tail.as_deref() == Ok(expected), "a range reads wrong")?;
            let past = place.read(file, u64::from(len) - 1, 2).await;
            ensure(past.is_err(), "a read past the end did not fail")?;
        }
        Ok(())
    }

    /// The chunks of two files that the checks fill completely: the first
    /// file, and the file of one byte at the end.
    fn committed_chunks(&self) -> impl Iterator<Item = u64> + Clone {
        let last = self.sample.chunks() - 1;
        self.sample.artifact.file_chunks(FIRST_FILE).chain([last])
    }

    /// The first chunk of a file the checks leave unfinished.
    fn unfinished_chunk(&self) -> u64 {
        self.sample.artifact.file_chunks(SECOND_FILE).start
    }

    async fn resume(&self, name: &str) -> Result<Arc<Store>, Wrong> {
        let place = (self.open)(&self.sample.artifact, name);
        Store::resume(self.sample.artifact.clone(), place, |_| {})
            .await
            .map_err(|e| wrong("opening a place", &e))
    }

    async fn put(&self, store: &Arc<Store>, index: u64) -> Result<(), Wrong> {
        let kept = store.write_chunk(index, self.sample.chunk(index)).await;
        let kept = kept.map_err(|e| wrong("writing a chunk", &e))?;
        ensure(
            kept != Put::Refused,
            "the right bytes for a chunk were refused",
        )
    }

    async fn same(&self, store: &Store, index: u64) -> Result<(), Wrong> {
        let read = store.read_chunk(index).await;
        let read = read.map_err(|e| wrong("reading back a chunk", &e))?;
        ensure(
            read == self.sample.chunk(index),
            "a chunk reads back changed",
        )
    }
}

fn ensure(holds: bool, otherwise: &str) -> Result<(), Wrong> {
    if holds {
        Ok(())
    } else {
        Err(otherwise.to_owned())
    }
}

fn wrong(doing: &str, failure: &Failure) -> Wrong {
    format!("{doing} failed: {failure}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    async fn filled() -> (Sample, Arc<Memory>, Arc<Store>) {
        let sample = Sample::new().unwrap();
        let place = Arc::new(Memory::new(sample.artifact.clone()));
        let store = Store::resume(sample.artifact.clone(), place.clone(), |_| {})
            .await
            .unwrap();
        (sample, place, store)
    }

    async fn put(store: &Arc<Store>, sample: &Sample, index: u64) -> Put {
        let put = store.write_chunk(index, sample.chunk(index)).await.unwrap();
        assert_ne!(put, Put::Refused);
        put
    }

    #[tokio::test]
    async fn a_tree_in_memory_is_a_place() {
        let places: Mutex<HashMap<String, Memory>> = Mutex::default();
        let checked = check(|artifact, name| {
            let mut places = places.lock().unwrap();
            let place = places
                .entry(name.to_owned())
                .or_insert_with(|| Memory::new(artifact.clone()));
            Arc::new(place.clone())
        });
        assert_eq!(checked.await, Ok(()));
    }

    /// The checks are no use unless they notice: a place that loses what it
    /// was given does not pass.
    #[tokio::test]
    async fn a_place_that_forgets_does_not_pass() {
        let checked = check(|artifact, _| Arc::new(Memory::new(artifact.clone())));
        assert_eq!(
            checked.await,
            Err("a finished tree is not found again".to_owned())
        );
    }

    #[tokio::test]
    async fn a_committed_file_is_the_bytes_that_were_sent() {
        let (sample, place, store) = filled().await;
        // The file with no bytes is there from the start.
        assert_eq!(place.committed(2), Some(Vec::new()));
        let chunks = sample.artifact.file_chunks(FIRST_FILE);
        for index in chunks.clone() {
            assert_eq!(place.committed(FIRST_FILE), None, "committed too soon");
            // The store says which chunk made the file whole.
            let last = index + 1 == chunks.end;
            let expected = if last {
                Put::Completed(FIRST_FILE)
            } else {
                Put::Stored
            };
            assert_eq!(put(&store, &sample, index).await, expected);
        }
        // Sent again, it is a chunk the store has, and completes nothing.
        assert_eq!(put(&store, &sample, chunks.start).await, Put::Stored);
        assert_eq!(
            place.committed(FIRST_FILE).as_ref(),
            Some(&sample.files[FIRST_FILE])
        );
        assert_eq!(place.committed(SECOND_FILE), None);
    }

    #[tokio::test]
    async fn a_place_that_is_only_read_holds_everything() {
        let (sample, place, store) = filled().await;
        for index in 0..sample.chunks() {
            put(&store, &sample, index).await;
        }

        assert!(!place.is_finished());
        store.finish().await.unwrap();
        assert!(place.is_finished(), "the place was never told to finish");

        let whole = Store::whole(sample.artifact.clone(), place);
        assert!(whole.is_complete());
        assert_eq!(whole.read_chunk(0).await, Ok(sample.chunk(0)));
        // A chunk sent to it is one it has.
        put(&whole, &sample, 0).await;
        whole.finish().await.unwrap();
    }

    #[tokio::test]
    async fn what_was_examined_is_reported() {
        use std::sync::atomic::AtomicU64;
        let sample = Sample::new().unwrap();
        let place = Arc::new(Memory::new(sample.artifact.clone()));
        let seen = Arc::new(AtomicU64::new(0));
        let count = seen.clone();
        let examined = move |bytes| {
            count.fetch_add(bytes, Ordering::Relaxed);
        };
        Store::resume(sample.artifact.clone(), place, examined)
            .await
            .unwrap();
        assert_eq!(seen.load(Ordering::Relaxed), sample.artifact.total_size());
    }
}
