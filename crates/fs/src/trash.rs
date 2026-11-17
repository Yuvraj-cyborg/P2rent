//! Where a sync puts what it takes away from a folder.
//!
//! Nothing a sync removes or replaces is gone at once. It is moved into a
//! folder inside the destination, one folder for each sync, under the path
//! it had. A person who synced from the wrong source finds their files
//! there.

use model::artifact::{EntryMetadata, Timestamp};
use model::mirror::TRASH;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

use crate::io as fs_io;

/// The trash of one sync into one folder.
#[derive(Debug, Clone)]
pub struct Trash {
    /// The folder that is being synced into.
    root: PathBuf,
    /// Where this sync's trash goes: a folder of its own inside the trash.
    at: PathBuf,
}

impl Trash {
    /// The trash of a sync into the folder `root`, which began at `began`.
    /// Nothing is made until something is put in it.
    pub fn new(root: &Path, began: Timestamp) -> Self {
        let at = root
            .join(TRASH)
            .join(format!("{}.{:09}", began.seconds, began.nanos));
        Self {
            root: root.to_owned(),
            at,
        }
    }

    /// The folder this sync's trash goes to.
    pub fn folder(&self) -> &Path {
        &self.at
    }

    fn paths(&self, relative: &str) -> (PathBuf, PathBuf) {
        let mut from = self.root.clone();
        let mut to = self.at.clone();
        // The paths of a tree were checked when the tree was made: every
        // part is a plain name.
        from.extend(relative.split('/'));
        to.extend(relative.split('/'));
        (from, to)
    }

    /// Moves what is at `relative`, a path of the tree with `/` between its
    /// parts, into the trash. A folder goes with everything in it. If
    /// nothing is there, nothing is done: it went with a folder above it.
    ///
    /// # Errors
    ///
    /// Returns an error if it cannot be moved.
    pub fn take(&self, relative: &str) -> io::Result<()> {
        let (from, to) = self.paths(relative);
        if std::fs::symlink_metadata(&from).is_err_and(|e| e.kind() == ErrorKind::NotFound) {
            return Ok(());
        }
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(&from, &to)
    }

    /// Gives the file at `relative` a second name in the trash, and leaves
    /// it where it is. When something is renamed over it, the file is
    /// still in the trash, and at no moment was its name without a file.
    ///
    /// Where the file system has no second names, the file is copied.
    ///
    /// # Errors
    ///
    /// Returns an error if the file can be neither linked nor copied.
    pub fn keep(&self, relative: &str) -> io::Result<()> {
        let (from, to) = self.paths(relative);
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if std::fs::hard_link(&from, &to).is_ok() {
            return Ok(());
        }
        std::fs::copy(&from, &to).map(drop)
    }
}

/// Takes away what is at `path` for good: a file, a link, or a folder with
/// everything in it. For a sync that was told to keep no trash.
///
/// # Errors
///
/// Returns an error if it cannot be removed. Nothing being there is not
/// one.
pub fn remove(path: &Path) -> io::Result<()> {
    let removed = match std::fs::symlink_metadata(path) {
        Ok(found) if found.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(e) => Err(e),
    };
    match removed {
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Gives the file at `path` the permissions recorded for it. Its time is
/// left: a file with another time is replaced, not touched.
///
/// # Errors
///
/// Returns an error if the file cannot be opened or changed.
pub fn touch(path: &Path, recorded: EntryMetadata) -> io::Result<()> {
    let file = std::fs::File::open(path)?;
    fs_io::set_mode(&file, recorded.mode)
}
