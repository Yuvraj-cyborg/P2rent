use super::io;
use model::artifact::{Artifact, Bitfield, Hash, Kind, PART_SUFFIX};
use model::assembly::Assembly;
use rayon::prelude::*;
use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::io::{Error, ErrorKind, Result};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};

/// An artifact's files on disk, addressed by chunk index.
///
/// A file that is whole lives under its final name. A file that is still
/// being filled lives in a part file beside it, named with
/// [`PART_SUFFIX`]. It is flushed to disk and renamed into place only when
/// every one of its chunks has been verified. So a file under a final name
/// is always complete and correct, however the program stops, and a file
/// that was already there is replaced in one step or left alone, never
/// changed in place.
///
/// Chunks can be read from either kind of file, so a node serves what it
/// holds while it is still downloading the rest.
#[derive(Debug)]
pub struct DiskStore {
    artifact: Arc<Artifact>,
    root: PathBuf,
    /// Which chunks are on disk and verified, and which files are whole.
    assembly: RwLock<Assembly>,
    files: Vec<FileState>,
    /// Directories that have had a file renamed into them since they were
    /// last flushed. See [`DiskStore::flush`].
    unflushed: Mutex<BTreeSet<PathBuf>>,
}

#[derive(Debug)]
struct FileState {
    /// Whether the file has its final name. Set after the rename, so a
    /// reader that sees it finds the file there.
    in_place: AtomicBool,
    /// Held while a chunk is added to the file, so that the file is not
    /// renamed while another chunk is being written into it.
    writing: Mutex<()>,
}

impl FileState {
    fn new(in_place: bool) -> Self {
        Self {
            in_place: AtomicBool::new(in_place),
            writing: Mutex::new(()),
        }
    }
}

impl DiskStore {
    /// A store over files that are already whole, as after indexing them.
    /// Nothing is read: the caller vouches for the content.
    ///
    /// `root` is the file itself for a file artifact, or the directory that
    /// holds the tree for a directory artifact.
    pub fn whole(artifact: Arc<Artifact>, root: PathBuf) -> Self {
        let held = Bitfield::full(artifact.chunk_count());
        let files = artifact.manifest().files.len();
        Self {
            assembly: RwLock::new(Assembly::new(artifact.layout().clone(), held)),
            files: (0..files).map(|_| FileState::new(true)).collect(),
            unflushed: Mutex::default(),
            artifact,
            root,
        }
    }

    /// Opens the artifact inside `dir`, under its own name, to be filled in.
    ///
    /// Whatever is already there is hashed. A file under its final name
    /// stays as it is if every chunk of it is right. If not, it is left
    /// untouched until its replacement is whole, and the chunks of it that
    /// are right are copied into the part file instead of being fetched. A
    /// part file keeps each chunk that is right, so an interrupted download
    /// carries on from where it stopped.
    ///
    /// `on_bytes` is called from worker threads with the size of each chunk
    /// examined.
    ///
    /// # Errors
    ///
    /// Returns an error if a file cannot be read or created, or if something
    /// other than a regular file is in the way.
    pub fn resume(
        artifact: Arc<Artifact>,
        dir: &Path,
        on_bytes: &(dyn Fn(u64) + Sync),
    ) -> Result<Self> {
        let root = dir.join(&artifact.manifest().name);
        let files = artifact.manifest().files.len();
        let mut store = Self {
            assembly: RwLock::new(Assembly::new(
                artifact.layout().clone(),
                Bitfield::new(artifact.chunk_count()),
            )),
            files: Vec::new(),
            unflushed: Mutex::default(),
            artifact,
            root,
        };
        if store.artifact.manifest().kind == Kind::Directory {
            std::fs::create_dir_all(&store.root)?;
        }

        let found: Vec<Found> = (0..files)
            .into_par_iter()
            .map(|file| store.survey(file, on_bytes))
            .collect::<Result<_>>()?;

        let mut held = Bitfield::new(store.artifact.chunk_count());
        for file in &found {
            for &index in &file.valid {
                held.set(index);
            }
        }
        store.files = found
            .iter()
            .map(|file| FileState::new(file.in_place))
            .collect();
        store.assembly = RwLock::new(Assembly::new(store.artifact.layout().clone(), held));

        // A part file with every chunk right is a download that stopped just
        // before its rename. A file with no chunks has nothing to wait for.
        for file in 0..files {
            if !store.is_in_place(file) && store.is_whole(file) {
                store.finish(file)?;
            }
        }
        Ok(store)
    }

    /// The artifact this store holds.
    pub fn artifact(&self) -> &Arc<Artifact> {
        &self.artifact
    }

    /// The file itself, or the directory holding the tree.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Whether chunk `index` is on disk and verified.
    pub fn has(&self, index: u64) -> bool {
        self.assembly().has(index)
    }

    /// A snapshot of which chunks are on disk and verified.
    pub fn held(&self) -> Bitfield {
        self.assembly().held().clone()
    }

    /// Whether every chunk is on disk and verified, which is also when every
    /// file has its final name.
    pub fn is_complete(&self) -> bool {
        self.assembly().is_complete()
    }

    /// Reads one chunk from disk.
    ///
    /// # Errors
    ///
    /// Returns an error if the chunk is not held or the file cannot be read.
    pub fn read_chunk(&self, index: u64) -> Result<Vec<u8>> {
        let location = self.artifact.locate(index).ok_or_else(out_of_range)?;
        if !self.has(index) {
            return Err(Error::new(ErrorKind::NotFound, "chunk is not held"));
        }
        let file = self.open(location.file)?;
        let mut buf = vec![0; location.len as usize];
        io::read_exact_at(&file, &mut buf, location.offset)?;
        Ok(buf)
    }

    /// Checks `data` against the manifest and, if it is chunk `index`, writes
    /// it to its file. When that makes the file whole, the file is flushed
    /// and renamed into place. Call [`DiskStore::flush`] once the last chunk
    /// is in, to make the renames last.
    ///
    /// Returns `false`, having written nothing, if `data` is not the chunk.
    ///
    /// # Errors
    ///
    /// Returns an error if the index is out of range or the file cannot be
    /// written, flushed or renamed.
    pub fn write_chunk(&self, index: u64, data: &[u8]) -> Result<bool> {
        let location = self.artifact.locate(index).ok_or_else(out_of_range)?;
        if Some(Hash::of(data)) != self.artifact.chunk_hash(index) {
            return Ok(false);
        }

        let state = &self.files[location.file];
        let _writing = state.writing.lock().unwrap_or_else(PoisonError::into_inner);
        if self.has(index) {
            return Ok(true);
        }
        let part = OpenOptions::new()
            .write(true)
            .open(self.part_path(location.file))?;
        io::write_all_at(&part, data, location.offset)?;
        drop(part);

        let whole = self
            .assembly
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .add(index);
        if let Some(file) = whole {
            self.finish(file)?;
        }
        Ok(true)
    }

    /// Flushes every directory that has had a file renamed into it, so that
    /// the renames survive a power cut.
    ///
    /// A file's bytes are flushed before it is renamed, so a power cut never
    /// leaves a file with holes under a final name. What it can do, until
    /// this is called, is undo a rename: the file is then found whole under
    /// its part name by the next [`DiskStore::resume`], which renames it
    /// again. Flushing a directory is slow, which is why it is done once
    /// here and not once for each file.
    ///
    /// # Errors
    ///
    /// Returns an error if a directory cannot be flushed.
    pub fn flush(&self) -> Result<()> {
        let renamed_into = std::mem::take(
            &mut *self
                .unflushed
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
        for dir in renamed_into {
            io::sync_dir(&dir)?;
        }
        Ok(())
    }

    /// Gives a whole file its final name: flushes it, then renames it over
    /// whatever is there.
    fn finish(&self, file: usize) -> Result<()> {
        let part = self.part_path(file);
        let written = OpenOptions::new().write(true).open(&part)?;
        if self.artifact.manifest().files[file].executable {
            io::make_executable(&written)?;
        }
        written.sync_all()?;
        drop(written);

        let path = self.final_path(file);
        std::fs::rename(&part, &path)?;
        if let Some(parent) = path.parent() {
            self.unflushed
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(parent.to_owned());
        }
        self.files[file].in_place.store(true, Ordering::Release);
        Ok(())
    }

    /// Finds out what is already on disk for one file, and makes sure there
    /// is a file to write the rest into.
    fn survey(&self, file: usize, on_bytes: &(dyn Fn(u64) + Sync)) -> Result<Found> {
        let size = self.artifact.manifest().files[file].size;
        let chunks = self.artifact.file_chunks(file);
        let path = self.final_path(file);
        let part = self.part_path(file);

        // Whatever has the final name is only ever read.
        let existing = open_regular(&path)?;
        let reusable = match &existing {
            Some(old) => self.verify(file, old, on_bytes)?,
            None => Vec::new(),
        };
        if let Some(old) = &existing
            && reusable.len() as u64 == chunks.end - chunks.start
            && old.metadata()?.len() == size
        {
            // A part file beside a whole file is left over from a download
            // of something else.
            remove_if_present(&part)?;
            return Ok(Found {
                valid: reusable,
                in_place: true,
            });
        }

        if let Some(parent) = part.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let started = open_regular(&part)?.is_some();
        let target = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&part)?;
        if target.metadata()?.len() != size {
            target.set_len(size)?;
        }
        // Nothing has been written to a new part file, so there is nothing
        // in it to check.
        let mut valid = if started {
            self.verify(file, &target, on_bytes)?
        } else {
            Vec::new()
        };

        // A chunk that is right in the old file is copied across, so that
        // only what is wrong or missing has to be fetched.
        if let Some(old) = &existing {
            let copied = reusable
                .into_par_iter()
                .filter(|index| valid.binary_search(index).is_err())
                .map(|index| Ok(self.copy(index, old, &target)?.then_some(index)))
                .collect::<Result<Vec<Option<u64>>>>()?;
            valid.extend(copied.into_iter().flatten());
        }
        Ok(Found {
            valid,
            in_place: false,
        })
    }

    /// The chunks of one file that are right in `on_disk`. A chunk that
    /// reaches past the end of `on_disk` is not.
    fn verify(
        &self,
        file: usize,
        on_disk: &File,
        on_bytes: &(dyn Fn(u64) + Sync),
    ) -> Result<Vec<u64>> {
        let len = on_disk.metadata()?.len();
        let valid = self
            .artifact
            .file_chunks(file)
            .into_par_iter()
            .map_init(Vec::new, |buf: &mut Vec<u8>, index| {
                let location = self.artifact.locate(index).ok_or_else(out_of_range)?;
                on_bytes(u64::from(location.len));
                if location.offset + u64::from(location.len) > len {
                    return Ok(None);
                }
                buf.resize(location.len as usize, 0);
                io::read_exact_at(on_disk, buf, location.offset)?;
                let matches = Some(Hash::of(buf)) == self.artifact.chunk_hash(index);
                Ok(matches.then_some(index))
            })
            .collect::<Result<Vec<Option<u64>>>>()?;
        Ok(valid.into_iter().flatten().collect())
    }

    /// Copies chunk `index` from one file to the same place in another, if
    /// it is still right when read. Returns whether it was copied.
    fn copy(&self, index: u64, from: &File, to: &File) -> Result<bool> {
        let location = self.artifact.locate(index).ok_or_else(out_of_range)?;
        let mut buf = vec![0; location.len as usize];
        io::read_exact_at(from, &mut buf, location.offset)?;
        if Some(Hash::of(&buf)) != self.artifact.chunk_hash(index) {
            return Ok(false);
        }
        io::write_all_at(to, &buf, location.offset)?;
        Ok(true)
    }

    /// Opens whichever file holds `file`'s bytes at the moment.
    fn open(&self, file: usize) -> Result<File> {
        if self.is_in_place(file) {
            return File::open(self.final_path(file));
        }
        match File::open(self.part_path(file)) {
            // It was renamed into place between the check and the open.
            Err(e) if e.kind() == ErrorKind::NotFound && self.is_in_place(file) => {
                File::open(self.final_path(file))
            }
            opened => opened,
        }
    }

    fn is_in_place(&self, file: usize) -> bool {
        self.files[file].in_place.load(Ordering::Acquire)
    }

    fn is_whole(&self, file: usize) -> bool {
        self.assembly().is_whole(file)
    }

    fn assembly(&self) -> std::sync::RwLockReadGuard<'_, Assembly> {
        self.assembly.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn final_path(&self, file: usize) -> PathBuf {
        match self.artifact.manifest().kind {
            Kind::File => self.root.clone(),
            // Paths were validated with the manifest: every component is a
            // plain name, so the result stays inside `root`.
            Kind::Directory => {
                let mut path = self.root.clone();
                path.extend(self.artifact.manifest().files[file].path.split('/'));
                path
            }
        }
    }

    /// Where `file` is written until it is whole: beside its final place, so
    /// that the rename never crosses a file system. No name in a manifest
    /// ends with the suffix, so this is never another file's final place.
    fn part_path(&self, file: usize) -> PathBuf {
        let mut name = self.final_path(file).into_os_string();
        name.push(PART_SUFFIX);
        PathBuf::from(name)
    }
}

/// What was found on disk for one file.
struct Found {
    /// The chunks that are right, in whichever file was looked at.
    valid: Vec<u64>,
    /// Whether that was the file under its final name.
    in_place: bool,
}

/// Opens `path` for reading, or returns `None` if nothing is there.
fn open_regular(path: &Path) -> Result<Option<File>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    if file.metadata()?.is_file() {
        Ok(Some(file))
    } else {
        Err(Error::new(
            ErrorKind::AlreadyExists,
            format!("{} exists and is not a regular file", path.display()),
        ))
    }
}

fn remove_if_present(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

fn out_of_range() -> Error {
    Error::new(ErrorKind::InvalidInput, "chunk index out of range")
}
