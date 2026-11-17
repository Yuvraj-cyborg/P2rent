use model::artifact::{Artifact, Bitfield, Hash};
use model::assembly::Assembly;
use model::place::{Failure, Sink, Source};
use std::sync::{Arc, Mutex, PoisonError, RwLock, RwLockReadGuard};

/// An artifact's chunks in a place, addressed by chunk index.
///
/// The place moves bytes. The store does what is the same for every place:
/// it checks a chunk against the manifest before the place is given it,
/// knows which chunks are held, and has a file committed the moment its last
/// chunk is in. `model::assembly`, which is proven, makes that last call.
#[derive(Debug)]
pub struct Store {
    artifact: Arc<Artifact>,
    source: Arc<dyn Source>,
    /// `None` for a place that holds everything already and is only read.
    sink: Option<Arc<dyn Sink>>,
    /// Which chunks are in the place and verified, and which files are
    /// whole.
    assembly: RwLock<Assembly>,
    /// One for each entry, held while a chunk is added to that file, so the
    /// file is not committed while another chunk is being written into it.
    writing: Vec<Mutex<()>>,
}

impl Store {
    /// A store over a place that holds every chunk already, as after
    /// indexing it. Nothing is read: the caller vouches for the content.
    pub fn whole(artifact: Arc<Artifact>, source: Arc<dyn Source>) -> Arc<Self> {
        let held = Bitfield::full(artifact.chunk_count());
        Self::over(artifact, source, None, held)
    }

    /// Opens a place to be filled in. The place is made ready, and what it
    /// already holds counts as held. `examined` is called with the size of
    /// each chunk the place looks at.
    ///
    /// # Errors
    ///
    /// Returns the place's failure if it cannot be made ready.
    pub async fn resume<P: Source + Sink + 'static>(
        artifact: Arc<Artifact>,
        place: Arc<P>,
        examined: impl Fn(u64) + Send + Sync + 'static,
    ) -> Result<Arc<Self>, Failure> {
        let opened = place.clone();
        let held = waiting(move || opened.begin(&examined)).await?;
        Ok(Self::over(artifact, place.clone(), Some(place), held))
    }

    fn over(
        artifact: Arc<Artifact>,
        source: Arc<dyn Source>,
        sink: Option<Arc<dyn Sink>>,
        held: Bitfield,
    ) -> Arc<Self> {
        Arc::new(Self {
            // A set of the wrong length describes nothing, and the assembly
            // then starts from nothing held.
            assembly: RwLock::new(Assembly::new(artifact.layout().clone(), held)),
            writing: artifact.entries().iter().map(|_| Mutex::new(())).collect(),
            artifact,
            source,
            sink,
        })
    }

    /// The artifact this store holds.
    pub fn artifact(&self) -> &Arc<Artifact> {
        &self.artifact
    }

    /// Whether chunk `index` is in the place and verified.
    pub fn has(&self, index: u64) -> bool {
        self.assembly().has(index)
    }

    /// A snapshot of which chunks are in the place and verified.
    pub fn held(&self) -> Bitfield {
        self.assembly().held().clone()
    }

    /// Whether every chunk is in the place and verified, which is also when
    /// every file has been committed.
    pub fn is_complete(&self) -> bool {
        self.assembly().is_complete()
    }

    /// Reads one chunk from the place.
    ///
    /// # Errors
    ///
    /// Returns [`Failure::Absent`] if the chunk is not held, and the place's
    /// failure if it cannot be read.
    pub async fn read_chunk(&self, index: u64) -> Result<Vec<u8>, Failure> {
        let location = self.artifact.locate(index).ok_or_else(out_of_range)?;
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

    /// Checks `data` against the manifest and, if it is chunk `index`, has
    /// the place write it. When that makes a file whole, the file is
    /// committed. Call [`Store::finish`] once the last chunk is in.
    ///
    /// Returns `false`, having written nothing, if `data` is not the chunk.
    ///
    /// Once started, the work runs to its end even if the caller stops
    /// waiting for it, so a file is never counted as whole and left
    /// uncommitted.
    ///
    /// # Errors
    ///
    /// Returns a failure if the index is out of range, or if the place
    /// cannot write the chunk or commit the file.
    pub async fn write_chunk(self: &Arc<Self>, index: u64, data: Vec<u8>) -> Result<bool, Failure> {
        let store = self.clone();
        waiting(move || store.put(index, &data)).await
    }

    /// Hashes, writes and commits on the thread it is called from.
    fn put(&self, index: u64, data: &[u8]) -> Result<bool, Failure> {
        let location = self.artifact.locate(index).ok_or_else(out_of_range)?;
        if Some(Hash::of(data)) != self.artifact.chunk_hash(index) {
            return Ok(false);
        }

        let _writing = self.writing[location.file]
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if self.has(index) {
            return Ok(true);
        }
        let Some(sink) = &self.sink else {
            return Err(Failure::Fatal("this place is only read from".into()));
        };
        sink.write(location.file, location.offset, data)?;
        let whole = self
            .assembly
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .add(index);
        if let Some(file) = whole {
            sink.commit(file)?;
        }
        Ok(true)
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

    fn assembly(&self) -> RwLockReadGuard<'_, Assembly> {
        self.assembly.read().unwrap_or_else(PoisonError::into_inner)
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
