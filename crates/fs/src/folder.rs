use super::io;
use model::artifact::{Bitfield, EntryKind, Hash, PART_SUFFIX, Tree};
use model::assembly::Assembly;
use model::place::{Capabilities, Expected, Failure, Pending, Sink, Source};
use rayon::prelude::*;
use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::io::{Error, ErrorKind, Result};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

/// A tree's files in a local folder.
///
/// A file that is whole lives under its final name. A file that is still
/// being filled lives in a part file beside it, named with
/// [`PART_SUFFIX`]. It is flushed to disk and renamed into place only when
/// it is committed. So a file under a final name is always complete,
/// however the program stops, and a file that was already there is replaced
/// in one step or left alone, never changed in place.
///
/// Bytes can be read from either kind of file, so a node serves what it
/// holds while it is still downloading the rest.
///
/// The folder is a [`Source`] and a [`Sink`]. Reading through [`Source`]
/// runs on a thread that may block, and so needs a Tokio runtime.
#[derive(Debug, Clone)]
pub struct Folder(Arc<Inner>);

#[derive(Debug)]
struct Inner {
    tree: Arc<Tree>,
    root: PathBuf,
    /// Whether each entry has its final name. Set after the rename, so a
    /// reader that sees it finds the file there.
    in_place: Vec<AtomicBool>,
    /// Directories that have had a file renamed into them since they were
    /// last flushed. See [`Sink::finish`].
    unflushed: Mutex<BTreeSet<PathBuf>>,
    /// Links the tree holds that this system cannot create.
    skipped_links: AtomicUsize,
}

impl Folder {
    /// Files that are already whole, as after indexing them. Nothing is
    /// read: the caller vouches for the content.
    ///
    /// `root` is the file itself for a tree that is one file, or the
    /// directory that holds the tree.
    pub fn whole(tree: Arc<Tree>, root: PathBuf) -> Self {
        Self::over(tree, root, true)
    }

    /// The tree inside `dir`, under its own name, to be filled in.
    /// Nothing is touched until [`Sink::begin`].
    pub fn inside(tree: Arc<Tree>, dir: &Path) -> Self {
        let root = dir.join(tree.name());
        Self::over(tree, root, false)
    }

    fn over(tree: Arc<Tree>, root: PathBuf, in_place: bool) -> Self {
        Self(Arc::new(Inner {
            in_place: (0..tree.entries().len())
                .map(|_| AtomicBool::new(in_place))
                .collect(),
            unflushed: Mutex::default(),
            skipped_links: AtomicUsize::new(0),
            tree,
            root,
        }))
    }

    /// The file itself, or the directory holding the tree.
    pub fn root(&self) -> &Path {
        &self.0.root
    }

    /// How many links of the tree were left out because this system
    /// cannot create them.
    pub fn skipped_links(&self) -> usize {
        self.0.skipped_links.load(Ordering::Relaxed)
    }

    /// Makes the folder ready to be filled in, and returns the chunks that
    /// are already there and right.
    ///
    /// Its directories are created, and so are its links, which are small
    /// enough to need no download. A directory must be a real directory: a
    /// link standing where one belongs is refused, so nothing is ever
    /// written through a link to somewhere else.
    ///
    /// Whatever is already there is hashed, and a chunk is right if it has
    /// the digest `expected` gives for it. A file under its final name
    /// stays as it is if every chunk of it is right. If not, it is left
    /// untouched until its replacement is whole, and the chunks of it that
    /// are right are copied into the part file instead of being fetched. A
    /// part file keeps each chunk that is right, so an interrupted download
    /// carries on from where it stopped.
    ///
    /// `examined` is called from worker threads with the size of each chunk
    /// examined.
    ///
    /// # Errors
    ///
    /// Returns an error if a file cannot be read or created, if something of
    /// another kind is in the way, or if this file system treats two of the
    /// tree's names as one.
    fn prepare(
        &self,
        expected: &Expected<'_>,
        examined: &(dyn Fn(u64) + Sync),
    ) -> Result<Bitfield> {
        let tree = &self.0.tree;
        let entries = tree.entries().len();

        // Directories first. Paths are in order, so each comes after the
        // directory that holds it.
        if let Some(dir) = self.0.root.parent() {
            std::fs::create_dir_all(dir)?;
        }
        for (entry, described) in tree.entries().iter().enumerate() {
            if described.kind == EntryKind::Directory {
                ensure_directory(&self.final_path(entry))?;
            }
        }

        let found: Vec<Found> = (0..entries)
            .into_par_iter()
            .map(|entry| self.survey(entry, expected, examined))
            .collect::<Result<_>>()?;

        let mut held = Bitfield::new(tree.chunk_count());
        for (entry, file) in found.iter().enumerate() {
            for &index in &file.valid {
                held.set(index);
            }
            self.0.in_place[entry].store(file.in_place, Ordering::Release);
        }
        self.check_distinct()?;

        // A part file with every chunk right is a download that stopped just
        // before its rename. A file with no chunks has nothing to wait for.
        let assembly = Assembly::new(tree.layout().clone(), held.clone());
        for entry in 0..entries {
            if !self.is_in_place(entry) && assembly.is_whole(entry) {
                self.put_in_place(entry)?;
            }
        }
        let mut skipped = 0;
        for (entry, described) in tree.entries().iter().enumerate() {
            if let EntryKind::Symlink { target } = &described.kind {
                skipped += usize::from(!self.place_link(entry, target)?);
            }
        }
        self.0.skipped_links.store(skipped, Ordering::Relaxed);
        Ok(held)
    }

    /// Reads `len` bytes of `file` at `offset`, from its part file or from
    /// the file under its final name, whichever holds it now.
    ///
    /// # Errors
    ///
    /// Returns an error if the file is not there or is too short.
    pub fn read_at(&self, file: usize, offset: u64, len: u32) -> Result<Vec<u8>> {
        let opened = self.open(file)?;
        let mut buf = vec![0; len as usize];
        io::read_exact_at(&opened, &mut buf, offset)?;
        Ok(buf)
    }

    /// Writes `data` into the part file of `file` at `offset`.
    ///
    /// # Errors
    ///
    /// Returns an error if the part file is not there or cannot be written.
    fn write_at(&self, file: usize, offset: u64, data: &[u8]) -> Result<()> {
        let part = OpenOptions::new().write(true).open(self.part_path(file))?;
        io::write_all_at(&part, data, offset)
    }

    /// Gives a file that has every byte its final name: sets its time and
    /// permissions, flushes it, then renames it over whatever is there.
    ///
    /// # Errors
    ///
    /// Returns an error if the part file cannot be changed, flushed or
    /// renamed.
    fn put_in_place(&self, file: usize) -> Result<()> {
        let part = self.part_path(file);
        let written = OpenOptions::new().write(true).open(&part)?;
        if let Some(metadata) = self.0.tree.metadata(file) {
            if let Some(time) = metadata.modified.and_then(io::system_time) {
                written.set_modified(time)?;
            }
            io::set_mode(&written, metadata.mode)?;
        }
        written.sync_all()?;
        drop(written);
        self.rename_into_place(file, &part)
    }

    /// Removes the part file of `file`. Whatever has the final name stays.
    ///
    /// # Errors
    ///
    /// Returns an error if the part file is there and cannot be removed.
    fn discard(&self, file: usize) -> Result<()> {
        remove_if_present(&self.part_path(file))
    }

    /// Finishes the tree: gives each directory its recorded time and
    /// permissions, then flushes every directory that has had a file renamed
    /// into it, so that the renames survive a power cut.
    ///
    /// A file's bytes are flushed before it is renamed, so a power cut never
    /// leaves a file with holes under a final name. What it can do, until
    /// this is called, is undo a rename: the file is then found whole under
    /// its part name the next time the folder is made ready, which renames it
    /// again. Flushing a directory is slow, which is why it is done once
    /// here and not once for each file.
    ///
    /// While a file is still without its final name, only the flush is done.
    ///
    /// # Errors
    ///
    /// Returns an error if a directory cannot be changed or flushed.
    fn seal(&self) -> Result<()> {
        let entries = self.0.tree.entries();
        if (0..entries.len()).all(|entry| self.is_in_place(entry)) {
            // Children before parents: filling a directory changes its
            // time, and one that may not be written cannot be filled.
            for (entry, described) in entries.iter().enumerate().rev() {
                if described.kind == EntryKind::Directory
                    && let Some(metadata) = self.0.tree.metadata(entry)
                {
                    io::restore_directory(&self.final_path(entry), metadata)?;
                }
            }
        }
        let renamed_into = std::mem::take(
            &mut *self
                .0
                .unflushed
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
        for dir in renamed_into {
            io::sync_dir(&dir)?;
        }
        Ok(())
    }

    /// Creates the link `entry` if this system can, replacing whatever has
    /// its name in one step. Returns `false` if this system cannot.
    #[cfg(unix)]
    fn place_link(&self, entry: usize, target: &str) -> Result<bool> {
        let path = self.final_path(entry);
        if std::fs::read_link(&path).is_ok_and(|existing| existing == Path::new(target)) {
            return Ok(true);
        }
        let part = self.part_path(entry);
        remove_if_present(&part)?;
        io::symlink(target, &part)?;
        self.rename_into_place(entry, &part)?;
        Ok(true)
    }

    /// Creating a link on Windows needs a privilege most accounts lack.
    #[cfg(not(unix))]
    fn place_link(&self, _entry: usize, _target: &str) -> Result<bool> {
        Ok(false)
    }

    fn rename_into_place(&self, entry: usize, part: &Path) -> Result<()> {
        let path = self.final_path(entry);
        std::fs::rename(part, &path)?;
        if let Some(parent) = path.parent() {
            self.0
                .unflushed
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(parent.to_owned());
        }
        self.0.in_place[entry].store(true, Ordering::Release);
        Ok(())
    }

    /// Refuses a destination where two of the tree's names are one file
    /// or one directory. That happens on a file system that ignores letter
    /// case or the form of accented letters, and two downloads writing into
    /// one file would both be wrong.
    fn check_distinct(&self) -> Result<()> {
        let mut seen = std::collections::HashMap::new();
        for entry in 0..self.0.tree.entries().len() {
            if let Some(identity) = self.identity(entry)?
                && let Some(other) = seen.insert(identity, entry)
            {
                return Err(self.clash(other, entry));
            }
        }
        Ok(())
    }

    /// What the file system itself tells one file or directory from another
    /// by: the device and the number it has there. `None` for an entry that
    /// needs no check.
    #[cfg(unix)]
    fn identity(&self, entry: usize) -> Result<Option<(u64, u64)>> {
        use std::os::unix::fs::MetadataExt;
        let on_disk = match self.0.tree.entries()[entry].kind {
            EntryKind::Directory => self.final_path(entry),
            // A part file is always this folder's own. A whole file that
            // was already there may be a hard link, which is harmless.
            EntryKind::File { .. } if !self.is_in_place(entry) => self.part_path(entry),
            EntryKind::File { .. } | EntryKind::Symlink { .. } => return Ok(None),
        };
        let found = std::fs::symlink_metadata(&on_disk)?;
        Ok(Some((found.dev(), found.ino())))
    }

    /// Windows ignores letter case, so there a path in small letters stands
    /// for every way of writing it.
    #[cfg(not(unix))]
    fn identity(&self, entry: usize) -> Result<Option<String>> {
        Ok(Some(self.0.tree.entries()[entry].path.to_lowercase()))
    }

    fn clash(&self, one: usize, other: usize) -> Error {
        let entries = self.0.tree.entries();
        Error::new(
            ErrorKind::AlreadyExists,
            format!(
                "{:?} and {:?} are the same name on this file system, which cannot hold both",
                entries[one].path, entries[other].path
            ),
        )
    }

    /// Finds out what is already on disk for one file, and makes sure there
    /// is a file to write the rest into.
    fn survey(
        &self,
        file: usize,
        expected: &Expected<'_>,
        examined: &(dyn Fn(u64) + Sync),
    ) -> Result<Found> {
        let tree = &self.0.tree;
        let EntryKind::File { size } = tree.entries()[file].kind else {
            // A directory is already there, and a link is made afterwards.
            return Ok(Found {
                valid: Vec::new(),
                in_place: true,
            });
        };
        let chunks = tree.file_chunks(file);
        let path = self.final_path(file);
        let part = self.part_path(file);

        // Whatever has the final name is only ever read.
        let existing = open_regular(&path)?;
        let reusable = match &existing {
            Some(old) => self.verify(file, old, expected, examined)?,
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
            self.verify(file, &target, expected, examined)?
        } else {
            Vec::new()
        };
        // A chunk that is right in the old file is copied across, so that
        // only what is wrong or missing has to be fetched.
        if let Some(old) = &existing {
            let copied = reusable
                .into_par_iter()
                .filter(|index| valid.binary_search(index).is_err())
                .map(|index| Ok(self.copy(index, expected, old, &target)?.then_some(index)))
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
        expected: &Expected<'_>,
        examined: &(dyn Fn(u64) + Sync),
    ) -> Result<Vec<u64>> {
        let tree = &self.0.tree;
        let len = on_disk.metadata()?.len();
        let valid = tree
            .file_chunks(file)
            .into_par_iter()
            .map_init(Vec::new, |buf: &mut Vec<u8>, index| {
                let location = tree.locate(index).ok_or_else(out_of_range)?;
                examined(u64::from(location.len));
                if location.offset + u64::from(location.len) > len {
                    return Ok(None);
                }
                buf.resize(location.len as usize, 0);
                io::read_exact_at(on_disk, buf, location.offset)?;
                let matches = expected(index) == Some(Hash::of(buf));
                Ok(matches.then_some(index))
            })
            .collect::<Result<Vec<Option<u64>>>>()?;
        Ok(valid.into_iter().flatten().collect())
    }

    /// Copies chunk `index` from one file to the same place in another, if
    /// it is still right when read. Returns whether it was copied.
    fn copy(&self, index: u64, expected: &Expected<'_>, from: &File, to: &File) -> Result<bool> {
        let tree = &self.0.tree;
        let location = tree.locate(index).ok_or_else(out_of_range)?;
        let mut buf = vec![0; location.len as usize];
        io::read_exact_at(from, &mut buf, location.offset)?;
        if expected(index) != Some(Hash::of(&buf)) {
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
        self.0.in_place[file].load(Ordering::Acquire)
    }

    fn final_path(&self, entry: usize) -> PathBuf {
        let mut path = self.0.root.clone();
        // Paths were validated with the manifest: every name is a plain
        // name, so the result stays inside `root`. The root's own path is
        // empty.
        let relative = &self.0.tree.entries()[entry].path;
        if !relative.is_empty() {
            path.extend(relative.split('/'));
        }
        path
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

/// The result of an operation on a place.
type Placed<T> = std::result::Result<T, Failure>;

/// A file that cannot be read because it is not there is absent. Anything
/// else that goes wrong on a local disk is not cured by trying again.
fn unread(error: &Error) -> Failure {
    match error.kind() {
        ErrorKind::NotFound | ErrorKind::UnexpectedEof => Failure::Absent(error.to_string()),
        _ => Failure::Fatal(error.to_string()),
    }
}

/// Nothing that goes wrong writing to a local disk is cured by trying
/// again.
fn placed<T>(done: Result<T>) -> Placed<T> {
    done.map_err(|error| Failure::Fatal(error.to_string()))
}

impl Source for Folder {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            random_access: true,
        }
    }

    fn read(&self, file: usize, offset: u64, len: u32) -> Pending<'_, Vec<u8>> {
        let folder = self.clone();
        Box::pin(async move {
            // Reading a file may wait on the disk.
            let read = tokio::task::spawn_blocking(move || folder.read_at(file, offset, len));
            match read.await {
                Ok(read) => read.map_err(|error| unread(&error)),
                Err(stopped) => Err(Failure::Fatal(format!(
                    "reading a local file was cut short: {stopped}"
                ))),
            }
        })
    }
}

impl Sink for Folder {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            random_access: true,
        }
    }

    fn begin(&self, expected: &Expected<'_>, examined: &(dyn Fn(u64) + Sync)) -> Placed<Bitfield> {
        placed(self.prepare(expected, examined))
    }

    fn write(&self, file: usize, offset: u64, data: &[u8]) -> Placed<()> {
        placed(self.write_at(file, offset, data))
    }

    fn commit(&self, file: usize) -> Placed<()> {
        placed(self.put_in_place(file))
    }

    fn abort(&self, file: usize) -> Placed<()> {
        placed(self.discard(file))
    }

    fn finish(&self) -> Placed<()> {
        placed(self.seal())
    }
}

/// What was found on disk for one file.
struct Found {
    /// The chunks that are right, in whichever file was looked at.
    valid: Vec<u64>,
    /// Whether that was the file under its final name.
    in_place: bool,
}

/// Makes sure `path` is a directory, creating it if nothing is there. A link
/// to a directory does not count: what is written under it would land
/// somewhere else.
fn ensure_directory(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(found) if found.is_dir() => Ok(()),
        Ok(_) => Err(Error::new(
            ErrorKind::AlreadyExists,
            format!("{} exists and is not a directory", path.display()),
        )),
        Err(e) if e.kind() == ErrorKind::NotFound => std::fs::create_dir(path),
        Err(e) => Err(e),
    }
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
