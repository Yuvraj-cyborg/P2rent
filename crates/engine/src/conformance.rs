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
    Artifact, Bitfield, Entry, EntryKind, EntryMetadata, Hash, MIN_CHUNK_SIZE, Manifest, Tree,
};
use model::place::{Capabilities, Description, Expected, Failure, Pending, Reader, Sink, Source};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

/// A tree held in memory. Clones share the bytes, so a clone opened again
/// with [`Sink::begin`] sees what an earlier transfer left, as a program
/// started again would.
#[derive(Debug, Clone)]
pub struct Memory {
    tree: Arc<Tree>,
    files: Arc<Mutex<Vec<Slot>>>,
    finished: Arc<AtomicBool>,
}

/// One entry's bytes: the version with its final name, and the one being
/// written.
#[derive(Debug, Default)]
struct Slot {
    committed: Option<Vec<u8>>,
    pending: Option<Vec<u8>>,
    /// What the engine has noted beside the pending version.
    notes: Vec<u8>,
}

impl Memory {
    /// An empty place for `tree`.
    pub fn new(tree: Arc<Tree>) -> Self {
        let files = tree.entries().iter().map(|_| Slot::default()).collect();
        Self {
            tree,
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
    fn right(&self, file: usize, bytes: &[u8], expected: &Expected<'_>) -> Vec<u64> {
        self.tree
            .file_chunks(file)
            .filter(|&index| {
                self.tree.locate(index).is_some_and(|at| {
                    let start = usize::try_from(at.offset).unwrap_or(usize::MAX);
                    let chunk = start
                        .checked_add(at.len as usize)
                        .and_then(|end| bytes.get(start..end));
                    chunk.is_some_and(|chunk| expected(index) == Some(Hash::of(chunk)))
                })
            })
            .collect()
    }

    fn open(&self, expected: &Expected<'_>, examined: &(dyn Fn(u64) + Sync)) -> Bitfield {
        let mut held = Bitfield::new(self.tree.chunk_count());
        for (file, entry) in self.tree.entries().iter().enumerate() {
            let EntryKind::File { size } = entry.kind else {
                continue;
            };
            examined(size);
            let size = usize::try_from(size).unwrap_or(usize::MAX);
            let chunks = self.tree.file_chunks(file);
            self.with(|files| {
                let slot = &mut files[file];
                let old = slot.committed.take().unwrap_or_default();
                let reusable = self.right(file, &old, expected);
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
                        if let Some(at) = self.tree.locate(index) {
                            let start = usize::try_from(at.offset).unwrap_or(usize::MAX);
                            let end = start + at.len as usize;
                            pending[start..end].copy_from_slice(&old[start..end]);
                        }
                    }
                    let right = self.right(file, &pending, expected);
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

/// `len` bytes of `bytes` from `offset`, if every one of them is there.
fn range_of(bytes: Option<&Vec<u8>>, offset: u64, len: u32) -> Option<Vec<u8>> {
    let start = usize::try_from(offset).ok()?;
    let end = start.checked_add(len as usize)?;
    bytes?.get(start..end).map(<[u8]>::to_vec)
}

/// Copies `data` into `buffer` at `offset`. A buffer that may `grow` is made
/// long enough to take it, with zeros where nothing was written. `None` if
/// the bytes do not fit.
fn put_at(buffer: &mut Vec<u8>, offset: u64, data: &[u8], grow: bool) -> Option<()> {
    let start = usize::try_from(offset).ok()?;
    let end = start.checked_add(data.len())?;
    if grow && buffer.len() < end {
        buffer.resize(end, 0);
    }
    buffer.get_mut(start..end)?.copy_from_slice(data);
    Some(())
}

fn ready<'a, T: Send + 'a>(result: Result<T, Failure>) -> Pending<'a, T> {
    Box::pin(std::future::ready(result))
}

impl Memory {
    /// The bytes of `file` from `offset`, `len` of them, as they are now.
    fn bytes(&self, file: usize, offset: u64, len: u32) -> Result<Vec<u8>, Failure> {
        self.slot(file, |slot| {
            // What is being written replaces what is committed, as a part
            // file does, so that is where a chunk just stored is found.
            let bytes = slot.pending.as_ref().or(slot.committed.as_ref());
            range_of(bytes, offset, len)
                .ok_or_else(|| Failure::Absent(format!("file {file} does not reach that far")))
        })
    }
}

impl Source for Memory {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            random_access: true,
        }
    }

    fn describe(&self) -> Pending<'_, Description> {
        ready(Ok(Description::of(&self.tree)))
    }

    fn open(&self, file: usize, offset: u64) -> Pending<'_, Box<dyn Reader>> {
        let reader: Box<dyn Reader> = Box::new(MemoryReader {
            place: self.clone(),
            file,
            offset,
        });
        // A file that is not there cannot be opened.
        ready(self.bytes(file, offset, 0).map(|_| reader))
    }
}

/// One file of a place in memory, read in order.
#[derive(Debug)]
struct MemoryReader {
    place: Memory,
    file: usize,
    offset: u64,
}

impl Reader for MemoryReader {
    fn read(&mut self, len: u32) -> Pending<'_, Vec<u8>> {
        let read = self.place.bytes(self.file, self.offset, len);
        if read.is_ok() {
            self.offset += u64::from(len);
        }
        ready(read)
    }
}

impl Sink for Memory {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            random_access: true,
        }
    }

    fn begin(
        &self,
        expected: &Expected<'_>,
        examined: &(dyn Fn(u64) + Sync),
    ) -> Result<Bitfield, Failure> {
        Ok(self.open(expected, examined))
    }

    fn write(&self, file: usize, offset: u64, data: &[u8]) -> Result<(), Failure> {
        self.slot(file, |slot| {
            let pending = slot.pending.as_mut();
            pending
                .and_then(|pending| put_at(pending, offset, data, false))
                .ok_or_else(|| Failure::Fatal(format!("file {file} has no room for that")))
        })
    }

    fn read_back(&self, file: usize, offset: u64, len: u32) -> Result<Vec<u8>, Failure> {
        self.slot(file, |slot| {
            range_of(slot.pending.as_ref(), offset, len)
                .ok_or_else(|| Failure::Absent(format!("file {file} was not written that far")))
        })
    }

    fn notes(&self, file: usize, limit: usize) -> Result<Vec<u8>, Failure> {
        self.slot(file, |slot| {
            Ok(slot.notes.iter().copied().take(limit).collect())
        })
    }

    fn note(&self, file: usize, offset: u64, data: &[u8]) -> Result<(), Failure> {
        self.slot(file, |slot| {
            put_at(&mut slot.notes, offset, data, true)
                .ok_or_else(|| Failure::Fatal(format!("file {file} has no room for that note")))
        })
    }

    fn commit(&self, file: usize) -> Result<(), Failure> {
        self.slot(file, |slot| {
            let nothing = || Failure::Fatal(format!("file {file} has nothing to commit"));
            slot.committed = Some(slot.pending.take().ok_or_else(nothing)?);
            slot.notes.clear();
            Ok(())
        })
    }

    fn abort(&self, file: usize) -> Result<(), Failure> {
        self.slot(file, |slot| {
            slot.pending = None;
            slot.notes.clear();
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
    F: Fn(&Arc<Tree>, &str) -> Arc<P>,
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
    run.a_read_gives_the_whole_range_or_fails().await?;
    run.notes_last_as_long_as_what_they_are_beside().await?;
    run.with_no_digest_nothing_is_held()
}

impl<P, F> Run<'_, F>
where
    P: Source + Sink + 'static,
    F: Fn(&Arc<Tree>, &str) -> Arc<P>,
{
    async fn a_new_place_holds_nothing(&self) -> Result<(), Wrong> {
        let store = self.resume("new").await?;
        ensure(store.held().count_ones() == 0, "a new place holds chunks")?;
        ensure(!store.is_complete(), "a new place is complete")
    }

    async fn chunks_arrive_intact_in_any_order(&self) -> Result<(), Wrong> {
        let mut order: Vec<u64> = (0..self.sample.chunks()).collect();
        let place = (self.open)(self.sample.artifact.tree(), "probe");
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
        let place = (self.open)(self.sample.artifact.tree(), "part");
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
        let place = (self.open)(self.sample.artifact.tree(), "together");
        let digests = &self.sample.artifact;
        let opened = place.begin(&|index| digests.chunk_hash(index), &|_| {});
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

    /// Notes are kept beside a file that is being written, apart from its
    /// bytes, and are gone once the file is committed or aborted.
    async fn notes_last_as_long_as_what_they_are_beside(&self) -> Result<(), Wrong> {
        let tree = self.sample.artifact.tree();
        let noted = |name: &str, file: usize, limit: usize| {
            let read = (self.open)(tree, name).notes(file, limit);
            read.map_err(|e| wrong("reading notes", &e))
        };
        ensure(
            noted("notes", FIRST_FILE, 100)?.is_empty(),
            "a new place has notes",
        )?;

        let store = self.resume("notes").await?;
        let place = (self.open)(tree, "notes");
        for (offset, text) in [(0, &b"abc"[..]), (5, &b"xy"[..])] {
            let written = place.note(FIRST_FILE, offset, text);
            written.map_err(|e| wrong("writing a note", &e))?;
        }
        // What was skipped over reads as zeros.
        let all = b"abc\0\0xy".to_vec();
        ensure(
            noted("notes", FIRST_FILE, 100)? == all,
            "notes read back changed",
        )?;
        ensure(
            noted("notes", FIRST_FILE, 4)? == all[..4],
            "more notes than asked for",
        )?;
        ensure(
            noted("notes", SECOND_FILE, 100)?.is_empty(),
            "one file has another's notes",
        )?;

        // Neither the notes nor the bytes of the file disturb the other.
        let chunks = self.sample.artifact.file_chunks(FIRST_FILE);
        for index in chunks.start..chunks.end - 1 {
            self.put(&store, index).await?;
        }
        let first = self.sample.chunk(chunks.start);
        let length = u32::try_from(first.len()).unwrap_or(u32::MAX);
        let written = place.read_back(FIRST_FILE, 0, length);
        ensure(written == Ok(first), "what was written reads back changed")?;
        ensure(
            noted("notes", FIRST_FILE, 100)? == all,
            "writing a chunk changed the notes",
        )?;
        drop(store);

        // They are still there when the place is made ready again.
        let store = self.resume("notes").await?;
        ensure(
            noted("notes", FIRST_FILE, 100)? == all,
            "notes were lost on opening again",
        )?;
        ensure(store.has(chunks.start), "a noted file lost a chunk")?;

        // Committing the file ends them, and leaves the file's own bytes.
        self.put(&store, chunks.end - 1).await?;
        ensure(
            noted("notes", FIRST_FILE, 100)?.is_empty(),
            "a committed file has notes",
        )?;
        for index in chunks {
            self.same(&store, index).await?;
        }
        let size = u64::try_from(self.sample.files[FIRST_FILE].len()).unwrap_or(u64::MAX);
        let past = (self.open)(tree, "notes").read(FIRST_FILE, size, 1).await;
        ensure(
            past.is_err(),
            "notes were left at the end of a committed file",
        )?;

        // So does aborting it.
        let noting = place.note(SECOND_FILE, 0, b"zzz");
        noting.map_err(|e| wrong("writing a note", &e))?;
        let aborted = place.abort(SECOND_FILE);
        aborted.map_err(|e| wrong("aborting a file", &e))?;
        ensure(
            noted("notes", SECOND_FILE, 100)?.is_empty(),
            "an aborted file has notes",
        )
    }

    /// A place holds a chunk only if it is told the digest the chunk has.
    /// Follows the check that finished the place named "together".
    fn with_no_digest_nothing_is_held(&self) -> Result<(), Wrong> {
        let place = (self.open)(self.sample.artifact.tree(), "together");
        let held = place.begin(&|_| None, &|_| {});
        let held = held.map_err(|e| wrong("opening a whole tree", &e))?;
        ensure(
            held.count_ones() == 0,
            "chunks are held that nothing vouches for",
        )?;

        // Told the digests of one file only, it holds that file's chunks.
        let (digests, first) = (&self.sample.artifact, self.committed_chunks());
        let chunks = digests.file_chunks(FIRST_FILE);
        let only_first = |index| chunks.contains(&index).then(|| digests.chunk_hash(index))?;
        let held = place.begin(&only_first, &|_| {});
        let held = held.map_err(|e| wrong("opening a whole tree", &e))?;
        let expected = chunks.end - chunks.start;
        ensure(
            held.count_ones() == expected,
            "the chunks vouched for are not held",
        )?;
        ensure(
            first.take(1).all(|index| held.get(index)),
            "the first chunk is not held",
        )
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
        let place = (self.open)(self.sample.artifact.tree(), name);
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

/// Runs the checks of a source on `source`, which must hold exactly the
/// tree and the bytes of `sample`.
///
/// # Errors
///
/// Returns what is wrong with the first check the source fails.
pub async fn check_source(source: &dyn Source, sample: &Sample) -> Result<(), Wrong> {
    let tree = sample.artifact.tree();
    let described = source.describe().await;
    let described = described.map_err(|e| wrong("describing the source", &e))?;
    ensure(
        described.name == tree.name(),
        "the source gives another name",
    )?;
    let entries: Vec<_> = described.entries.iter().map(|(entry, _)| entry).collect();
    let expected: Vec<_> = tree.entries().iter().collect();
    ensure(entries == expected, "the source describes another tree")?;
    let known = described.known.len() == tree.entries().len();
    ensure(
        known,
        "what is known beforehand is not listed for each entry",
    )?;

    let random = source.capabilities().random_access;
    for (file, bytes) in sample.files.iter().enumerate() {
        if bytes.is_empty() {
            continue;
        }
        // Read in order, in pieces of uneven sizes, a file is its bytes.
        let reader = source.open(file, 0).await;
        let mut reader = reader.map_err(|e| wrong("opening a file", &e))?;
        let mut read = Vec::new();
        for size in [1usize, 7, 4096, 5000].into_iter().cycle() {
            let left = bytes.len() - read.len();
            if left == 0 {
                break;
            }
            let piece = reader
                .read(u32::try_from(size.min(left)).unwrap_or(1))
                .await;
            read.extend(piece.map_err(|e| wrong("reading on", &e))?);
        }
        ensure(&read == bytes, "a file read in order is not its bytes")?;
        let past = reader.read(1).await;
        ensure(past.is_err(), "a reader went on past the end of its file")?;

        // Opened part of the way in, it gives the rest.
        let middle = bytes.len() / 2;
        let rest = u32::try_from(bytes.len() - middle).unwrap_or(u32::MAX);
        if random || middle == 0 {
            let reader = source.open(file, middle as u64).await;
            let mut reader = reader.map_err(|e| wrong("opening part of the way in", &e))?;
            let tail = reader.read(rest).await;
            ensure(
                tail.as_deref() == Ok(&bytes[middle..]),
                "the rest of a file reads wrong",
            )?;
        }
        let range = source.read(file, middle as u64, rest).await;
        ensure(
            range.as_deref() == Ok(&bytes[middle..]),
            "a range reads wrong",
        )?;
    }
    Ok(())
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
        let place = Arc::new(Memory::new(sample.artifact.tree().clone()));
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
        let checked = check(|tree, name| {
            let mut places = places.lock().unwrap();
            let place = places
                .entry(name.to_owned())
                .or_insert_with(|| Memory::new(tree.clone()));
            Arc::new(place.clone())
        });
        assert_eq!(checked.await, Ok(()));
    }

    /// The checks are no use unless they notice: a place that loses what it
    /// was given does not pass.
    #[tokio::test]
    async fn a_place_that_forgets_does_not_pass() {
        let checked = check(|tree, _| Arc::new(Memory::new(tree.clone())));
        assert_eq!(
            checked.await,
            Err("a finished tree is not found again".to_owned())
        );
    }

    #[tokio::test]
    async fn a_tree_in_memory_is_a_source() {
        let (sample, place, store) = filled().await;
        for index in 0..sample.chunks() {
            put(&store, &sample, index).await;
        }
        assert_eq!(check_source(&*place, &sample).await, Ok(()));
    }

    /// And the check notices a source that holds something else.
    #[tokio::test]
    async fn a_source_with_other_bytes_does_not_pass() {
        let (mut sample, place, store) = filled().await;
        for index in 0..sample.chunks() {
            put(&store, &sample, index).await;
        }
        sample.files[FIRST_FILE][9] ^= 1;
        let checked = check_source(&*place, &sample).await;
        assert_eq!(
            checked,
            Err("a file read in order is not its bytes".to_owned())
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
        let place = Arc::new(Memory::new(sample.artifact.tree().clone()));
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
