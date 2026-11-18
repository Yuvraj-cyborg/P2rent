use crate::sync;
use model::artifact::{Artifact, Bitfield, EntryKind, FileDigest, Hash, Tree};
use model::assembly::Assembly;
use model::notes::Notes;
use model::place::{Begun, Failure, Foreknown, Sink, Source, Verification};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

/// What became of a chunk handed to a [`Store`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Put {
    /// The bytes are not the chunk. Nothing was written.
    Refused,
    /// The chunk is stored.
    Stored,
    /// The chunk is stored, and it was the last of this file, which now has
    /// its final name.
    Completed(usize),
}

/// A tree's chunks in a place, addressed by chunk index.
///
/// The place moves bytes. The store does what is the same for every place:
/// it checks what arrives before the place is given it, knows which chunks
/// are held, and has a file committed the moment its last chunk is in.
/// `model::assembly`, which is proven, makes that last call.
///
/// The digest of each chunk is either known from the start, in which case a
/// chunk is compared with it before it is kept, or learned as the chunk
/// arrives. What can be said of the bytes at the end differs: see
/// [`Store::verification`].
#[derive(Debug)]
pub struct Store {
    tree: Arc<Tree>,
    digests: Digests,
    source: Arc<dyn Source>,
    /// `None` for a place that holds everything already and is only read.
    sink: Option<Arc<dyn Sink>>,
    /// Which chunks are in the place, and which files are whole.
    assembly: RwLock<Assembly>,
    /// The files the place named as it was made ready, until they are
    /// asked for.
    named: Mutex<Vec<usize>>,
    /// One for each entry, held while a chunk is added to that file, so the
    /// file is not committed while another chunk is being written into it.
    writing: Vec<Mutex<()>>,
    /// Set when a whole file did not have the digest given for it and was
    /// thrown away. What the store records as held is then no longer what
    /// the place holds, so it takes nothing more and is never complete.
    spoiled: OnceLock<Failure>,
}

#[derive(Debug)]
enum Digests {
    /// Known before the transfer began.
    Known(Arc<Artifact>),
    /// Not known. Each is recorded as its chunk is stored.
    Learned(Learned),
}

#[derive(Debug)]
struct Learned {
    /// The digest of each chunk that is stored, by chunk index.
    digests: Mutex<Vec<Option<Hash>>>,
    /// For each entry, what was known of it beforehand, and the notes its
    /// digests are written into if it has a validator.
    files: Vec<(Foreknown, Option<Notes>)>,
    /// The artifact the tree turned out to be, once every digest is in.
    artifact: OnceLock<Arc<Artifact>>,
}

impl Store {
    /// A store over a place that holds every chunk already, as after
    /// indexing it. Nothing is read: the caller vouches for the content.
    pub fn whole(artifact: Arc<Artifact>, source: Arc<dyn Source>) -> Arc<Self> {
        let begun = Begun {
            held: Bitfield::full(artifact.chunk_count()),
            named: Vec::new(),
        };
        let tree = artifact.tree().clone();
        Self::over(tree, Digests::Known(artifact), source, None, begun)
    }

    /// Opens a place to be filled in with an artifact. The place is made
    /// ready, and what it already holds counts as held. `examined` is
    /// called with the size of each chunk the place looks at.
    ///
    /// # Errors
    ///
    /// Returns the place's failure if it cannot be made ready.
    pub async fn resume<P: Source + Sink + 'static>(
        artifact: Arc<Artifact>,
        place: Arc<P>,
        examined: impl Fn(u64) + Send + Sync + 'static,
    ) -> Result<Arc<Self>, Failure> {
        let (opened, expected) = (place.clone(), artifact.clone());
        let begun =
            waiting(move || opened.begin(&|index| expected.chunk_hash(index), &examined)).await?;
        let tree = artifact.tree().clone();
        let digests = Digests::Known(artifact);
        Ok(Self::over(tree, digests, place.clone(), Some(place), begun))
    }

    /// Opens a place to be filled in with a tree whose digests are not
    /// known. They are learned as the chunks arrive, and noted in the place
    /// so that a transfer that is interrupted can carry on.
    ///
    /// `files` says, for each entry of the tree, what is known of it
    /// beforehand. What an earlier transfer left is kept only if it was
    /// noted for the same version of the same file, and if its bytes still
    /// have the digests that were noted.
    ///
    /// # Errors
    ///
    /// Returns the place's failure if it cannot be made ready.
    pub async fn learn<P: Source + Sink + 'static>(
        tree: Arc<Tree>,
        place: Arc<P>,
        files: Vec<Foreknown>,
        examined: impl Fn(u64) + Send + Sync + 'static,
    ) -> Result<Arc<Self>, Failure> {
        let (opened, outline) = (place.clone(), tree.clone());
        let (learned, begun) =
            waiting(move || Learned::open(&outline, &*opened, files, &examined)).await?;
        let digests = Digests::Learned(learned);
        Ok(Self::over(tree, digests, place.clone(), Some(place), begun))
    }

    fn over(
        tree: Arc<Tree>,
        digests: Digests,
        source: Arc<dyn Source>,
        sink: Option<Arc<dyn Sink>>,
        begun: Begun,
    ) -> Arc<Self> {
        Arc::new(Self {
            // A set of the wrong length describes nothing, and the assembly
            // then starts from nothing held.
            assembly: RwLock::new(Assembly::new(tree.layout().clone(), begun.held)),
            named: Mutex::new(begun.named),
            writing: tree.entries().iter().map(|_| Mutex::new(())).collect(),
            spoiled: OnceLock::new(),
            tree,
            digests,
            source,
            sink,
        })
    }

    /// The tree this store holds.
    pub fn tree(&self) -> &Arc<Tree> {
        &self.tree
    }

    /// The artifact this store holds: its tree with the digest of every
    /// chunk. `None` while digests are still being learned.
    pub fn artifact(&self) -> Option<Arc<Artifact>> {
        match &self.digests {
            Digests::Known(artifact) => Some(artifact.clone()),
            Digests::Learned(learned) => learned.artifact(&self.tree),
        }
    }

    /// Whether the digest of every chunk was known before anything was
    /// fetched. Each chunk in the place has then been compared with its
    /// own, whichever supplier it came from, so no two of them can be of
    /// different versions of a file.
    pub fn knows_every_digest(&self) -> bool {
        matches!(self.digests, Digests::Known(_))
    }

    /// How far the bytes in this store have been checked, once it is
    /// complete.
    pub fn verification(&self) -> Verification {
        match &self.digests {
            Digests::Known(_) => Verification::Verified,
            Digests::Learned(learned) => learned.verification(&self.tree),
        }
    }

    /// The files that were given their final names as the place was made
    /// ready: those that needed nothing fetched. They are given once, to
    /// whoever asks first, so that each is told of once.
    pub fn named_at_the_start(&self) -> Vec<usize> {
        std::mem::take(&mut sync::lock(&self.named))
    }

    /// Whether chunk `index` is in the place.
    pub fn has(&self, index: u64) -> bool {
        sync::read(&self.assembly).has(index)
    }

    /// A snapshot of which chunks are in the place.
    pub fn held(&self) -> Bitfield {
        sync::read(&self.assembly).held().clone()
    }

    /// Whether every chunk is in the place, which is also when every file
    /// has been committed.
    pub fn is_complete(&self) -> bool {
        self.spoiled.get().is_none() && sync::read(&self.assembly).is_complete()
    }

    /// Reads one chunk from the place.
    ///
    /// # Errors
    ///
    /// Returns [`Failure::Absent`] if the chunk is not held, and the place's
    /// failure if it cannot be read.
    pub async fn read_chunk(&self, index: u64) -> Result<Vec<u8>, Failure> {
        let location = self.tree.locate(index).ok_or_else(out_of_range)?;
        if !self.has(index) {
            return Err(Failure::Absent(format!("chunk {index} is not held")));
        }
        let data = self
            .source
            .read(location.file, location.offset, location.len)
            .await?;
        if data.len() != location.len as usize {
            return Err(Failure::Corrupt(format!(
                "asked for the {} bytes of chunk {index} and was given {}",
                location.len,
                data.len()
            )));
        }
        Ok(data)
    }

    /// Takes the bytes of chunk `index` and has the place write them. If
    /// the chunk's digest is known the bytes are compared with it first,
    /// and if not, their digest is learned. When the chunk makes a file
    /// whole, the file is committed. Call [`Store::finish`] once the last
    /// chunk is in.
    ///
    /// Once started, the work runs to its end even if the caller stops
    /// waiting for it, so a file is never counted as whole and left
    /// uncommitted.
    ///
    /// # Errors
    ///
    /// Returns a failure if the index is out of range, if the place cannot
    /// write the chunk or commit the file, or if a file that is whole does
    /// not have the digest that was given for it.
    pub async fn write_chunk(self: &Arc<Self>, index: u64, data: Vec<u8>) -> Result<Put, Failure> {
        let store = self.clone();
        waiting(move || store.put(index, &data)).await
    }

    /// Hashes, writes and commits on the thread it is called from.
    fn put(&self, index: u64, data: &[u8]) -> Result<Put, Failure> {
        let location = self.tree.locate(index).ok_or_else(out_of_range)?;
        let digest = Hash::of(data);
        let acceptable = match &self.digests {
            Digests::Known(artifact) => artifact.chunk_hash(index) == Some(digest),
            // Nothing says what the bytes should be, only how many.
            Digests::Learned(_) => data.len() == location.len as usize,
        };
        if !acceptable {
            return Ok(Put::Refused);
        }

        let _writing = sync::lock(&self.writing[location.file]);
        if let Some(failure) = self.spoiled.get() {
            return Err(failure.clone());
        }
        if self.has(index) {
            return Ok(Put::Stored);
        }
        let Some(sink) = &self.sink else {
            return Err(Failure::Fatal("this place is only read from".into()));
        };
        sink.write(location.file, location.offset, data)?;
        if let Digests::Learned(learned) = &self.digests {
            let first = self.tree.file_chunks(location.file).start;
            learned.record(&**sink, location.file, index, index - first, digest)?;
        }
        let Some(file) = sync::write(&self.assembly).add(index) else {
            return Ok(Put::Stored);
        };
        if let Digests::Learned(learned) = &self.digests
            && let Err(failure) = learned.check(&self.tree, &**sink, file)
        {
            return Err(self.spoiled.get_or_init(|| failure).clone());
        }
        sink.commit(file)?;
        Ok(Put::Completed(file))
    }

    /// Finishes the tree once the last chunk is in, and makes what was
    /// committed last through a power cut.
    ///
    /// # Errors
    ///
    /// Returns the place's failure if it cannot finish.
    pub async fn finish(&self) -> Result<(), Failure> {
        match self.sink.clone() {
            Some(sink) => waiting(move || sink.finish()).await,
            None => Ok(()),
        }
    }

    /// Throws away every file that has been written to and not committed.
    /// For when what was being read turned out to have changed: bytes of
    /// two versions cannot be told apart, so none of them is kept.
    ///
    /// # Errors
    ///
    /// Returns the place's failure if something cannot be thrown away.
    pub async fn abandon(&self) -> Result<(), Failure> {
        let Some(sink) = self.sink.clone() else {
            return Ok(());
        };
        let tree = self.tree.clone();
        waiting(move || {
            for (file, entry) in tree.entries().iter().enumerate() {
                if entry.is_file() {
                    sink.abort(file)?;
                }
            }
            Ok(())
        })
        .await
    }
}

impl Learned {
    /// Reads what an earlier transfer noted, makes the place ready, and
    /// starts the notes of this one.
    fn open(
        tree: &Tree,
        sink: &dyn Sink,
        files: Vec<Foreknown>,
        examined: &(dyn Fn(u64) + Sync),
    ) -> Result<(Self, Begun), Failure> {
        let mut digests = vec![None; usize::try_from(tree.chunk_count()).unwrap_or(usize::MAX)];
        let mut described = Vec::with_capacity(tree.entries().len());
        let mut files = files.into_iter();
        for (file, entry) in tree.entries().iter().enumerate() {
            let known = files.next().unwrap_or_default();
            let EntryKind::File { size } = entry.kind else {
                described.push((known, None));
                continue;
            };
            let chunks = tree.file_chunks(file);
            // The notes of this file and no more, whatever is there.
            let limit = Notes::longest(size, tree.chunk_size());
            let earlier = Notes::read(&sink.notes(file, limit)?)
                .filter(|earlier| earlier.are_about(size, tree.chunk_size(), &known.validator));
            match earlier {
                Some(earlier) => {
                    for index in chunks.clone() {
                        if let Some(noted) = slot_of(&mut digests, index) {
                            *noted = earlier.digest(index - chunks.start);
                        }
                    }
                }
                // What is there was written for another version of the
                // file, or for none that can be told. None of it is kept.
                None => sink.abort(file)?,
            }
            let notes = Notes::new(size, tree.chunk_size(), &known.validator);
            described.push((known, notes));
        }

        let noted = &digests;
        let expected = |index: u64| noted.get(usize::try_from(index).ok()?).copied()?;
        let begun = sink.begin(&expected, examined)?;
        let held = &begun.held;
        for (file, (_, notes)) in described.iter().enumerate() {
            // A file that is whole already was committed as the place was
            // made ready, and has nothing left to keep notes beside.
            let unfinished = tree.file_chunks(file).any(|index| !held.get(index));
            if let Some(notes) = notes.as_ref().filter(|_| unfinished) {
                sink.note(file, 0, &notes.header())?;
            }
        }
        // A digest that was noted for bytes that are no longer right says
        // nothing.
        for (index, digest) in (0u64..).zip(&mut digests) {
            if !held.get(index) {
                *digest = None;
            }
        }
        let learned = Self {
            digests: Mutex::new(digests),
            files: described,
            artifact: OnceLock::new(),
        };
        Ok((learned, begun))
    }

    /// Records the digest of a chunk that has just been written, in memory
    /// and in the notes of its file.
    fn record(
        &self,
        sink: &dyn Sink,
        file: usize,
        index: u64,
        chunk_of_file: u64,
        digest: Hash,
    ) -> Result<(), Failure> {
        if let Some(known) = slot_of(&mut sync::lock(&self.digests), index) {
            *known = Some(digest);
        }
        let notes = self.files.get(file).and_then(|(_, notes)| notes.as_ref());
        match notes.and_then(|notes| notes.slot(chunk_of_file, digest)) {
            Some((offset, bytes)) => sink.note(file, offset, &bytes),
            None => Ok(()),
        }
    }

    /// Checks a file that is whole against the digest that was given for
    /// it, if one was. A file that does not have it is thrown away.
    fn check(&self, tree: &Tree, sink: &dyn Sink, file: usize) -> Result<(), Failure> {
        let Some(expected) = self.files.get(file).and_then(|(known, _)| known.digest) else {
            return Ok(());
        };
        let mut hasher = Hasher::new(&expected);
        for index in tree.file_chunks(file) {
            let at = tree.locate(index).ok_or_else(out_of_range)?;
            hasher.update(&sink.read_back(file, at.offset, at.len)?);
        }
        let found = hasher.finish();
        if found == expected {
            return Ok(());
        }
        sink.abort(file)?;
        let path = tree.entries().get(file).map_or("", |entry| &entry.path);
        let name = if path.is_empty() { tree.name() } else { path };
        Err(Failure::Corrupt(format!(
            "{name} does not have the digest that was given for it: \
             expected {expected}, found {found}"
        )))
    }

    /// The artifact, once every digest has been learned.
    fn artifact(&self, tree: &Tree) -> Option<Arc<Artifact>> {
        if let Some(artifact) = self.artifact.get() {
            return Some(artifact.clone());
        }
        let digests: Option<Vec<Hash>> = sync::lock(&self.digests).iter().copied().collect();
        let artifact = Arc::new(tree.with_digests(digests?).ok()?);
        Some(self.artifact.get_or_init(|| artifact).clone())
    }

    /// Verified if a digest was given for every file that has bytes, and
    /// so was checked before the file was committed. Consistent otherwise.
    fn verification(&self, tree: &Tree) -> Verification {
        let files = tree.entries().iter().zip(&self.files);
        let every_one_given = files
            .filter(|(entry, _)| entry.size() > 0)
            .all(|(_, (known, _))| known.digest.is_some());
        if every_one_given {
            Verification::Verified
        } else {
            Verification::Consistent
        }
    }
}

/// The place of chunk `index` in a list with one place for each chunk.
fn slot_of(digests: &mut [Option<Hash>], index: u64) -> Option<&mut Option<Hash>> {
    digests.get_mut(usize::try_from(index).ok()?)
}

/// Computes a digest of the kind that was given, over a whole file.
enum Hasher {
    Blake3(Box<blake3::Hasher>),
    Sha256(Box<ring::digest::Context>),
}

impl Hasher {
    fn new(kind: &FileDigest) -> Self {
        match kind {
            FileDigest::Blake3(_) => Self::Blake3(Box::default()),
            FileDigest::Sha256(_) => {
                let context = ring::digest::Context::new(&ring::digest::SHA256);
                Self::Sha256(Box::new(context))
            }
        }
    }

    fn update(&mut self, bytes: &[u8]) {
        match self {
            Self::Blake3(hasher) => {
                hasher.update(bytes);
            }
            Self::Sha256(context) => context.update(bytes),
        }
    }

    fn finish(self) -> FileDigest {
        match self {
            Self::Blake3(hasher) => {
                FileDigest::Blake3(Hash::from_bytes(*hasher.finalize().as_bytes()))
            }
            Self::Sha256(context) => {
                let mut bytes = [0; 32];
                bytes.copy_from_slice(context.finish().as_ref());
                FileDigest::Sha256(bytes)
            }
        }
    }
}

fn out_of_range() -> Failure {
    Failure::Fatal("chunk index out of range".into())
}

/// Runs `work` on a thread that is allowed to wait for a disk or a network,
/// and to spend a while hashing.
async fn waiting<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, Failure> + Send + 'static,
) -> Result<T, Failure> {
    tokio::task::spawn_blocking(work)
        .await
        .unwrap_or_else(|stopped| {
            Err(Failure::Fatal(format!(
                "work on a place was cut short: {stopped}"
            )))
        })
}
