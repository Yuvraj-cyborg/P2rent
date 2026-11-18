use super::io;
use crate::error::{Error, Result};
use model::artifact::path::{depth, is_safe_target};
use model::artifact::{
    Artifact, Entry, EntryKind, EntryMetadata, Hash, Manifest, PERMISSION_BITS, Tree, is_part_name,
    to_usize,
};
use model::mirror::TRASH;
use rayon::prelude::*;
use std::fs::File;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// The result of hashing a local file or directory.
#[derive(Debug)]
pub struct Indexed {
    /// The artifact describing what was hashed.
    pub artifact: Artifact,
    /// Canonical path of the file or directory that was indexed.
    pub root: PathBuf,
    /// Entries that were left out: sockets, devices and the like, links that
    /// point outside the tree, and part files left by a download that has
    /// not finished.
    pub skipped: usize,
}

/// Something found on disk, and how the manifest will describe it.
struct Source {
    path: PathBuf,
    entry: Entry,
    metadata: EntryMetadata,
}

impl Source {
    fn new(path: PathBuf, entry_path: String, kind: EntryKind, found: &std::fs::Metadata) -> Self {
        // A link's own permissions and time mean nothing and are not
        // restored, so they are recorded the same everywhere.
        let metadata = match kind {
            EntryKind::Symlink { .. } => EntryMetadata {
                mode: PERMISSION_BITS,
                modified: None,
            },
            EntryKind::File { .. } | EntryKind::Directory => EntryMetadata {
                mode: io::mode_of(found),
                modified: io::modified_of(found),
            },
        };
        Self {
            path,
            entry: Entry {
                path: entry_path,
                kind,
            },
            metadata,
        }
    }
}

/// Hashes `path` into an artifact without copying any data.
///
/// Files, directories and links are recorded, with their permissions and
/// the time each was last changed. A link is recorded as a link and is not
/// followed.
///
/// `on_bytes` is called from worker threads with the size of each chunk as it
/// is hashed.
///
/// # Errors
///
/// Returns an error if the path cannot be read, is neither a file nor a directory, or contains
/// a name that a manifest cannot hold, such as one that is not UTF-8.
pub fn index(path: &Path, chunk_size: u32, on_bytes: &(dyn Fn(u64) + Sync)) -> Result<Indexed> {
    let (root, name) = named(path)?;
    let (sources, skipped) = walk(&root)?;

    let per_file: Vec<Vec<Hash>> = sources
        .par_iter()
        .map(|source| match source.entry.kind {
            EntryKind::File { size } => hash_file(&source.path, size, chunk_size, on_bytes),
            EntryKind::Directory | EntryKind::Symlink { .. } => Ok(Vec::new()),
        })
        .collect::<Result<_>>()?;

    let manifest = Manifest::new(
        name,
        chunk_size,
        sources
            .into_iter()
            .map(|source| (source.entry, source.metadata))
            .collect(),
        per_file.into_iter().flatten().collect(),
    );
    Ok(Indexed {
        artifact: Artifact::new(manifest)?,
        root,
        skipped,
    })
}

/// What is at a path, without the digest of any of it.
#[derive(Debug)]
pub struct Scanned {
    /// The files, directories and links that were found.
    pub tree: Tree,
    /// Canonical path of the file or directory that was looked at.
    pub root: PathBuf,
    /// Entries that were left out, as [`Indexed::skipped`] counts them.
    pub skipped: usize,
}

/// Looks at what is at `path` and reads none of it: the files, directories
/// and links, with their sizes, permissions and times, as [`index`] records
/// them.
///
/// # Errors
///
/// As for [`index`].
pub fn scan(path: &Path, chunk_size: u32) -> Result<Scanned> {
    let (root, name) = named(path)?;
    let (sources, skipped) = walk(&root)?;
    let entries = sources
        .into_iter()
        .map(|source| (source.entry, source.metadata))
        .collect();
    Ok(Scanned {
        tree: Tree::new(name, chunk_size, entries)?,
        root,
        skipped,
    })
}

/// `tree` as a folder on this system keeps it.
///
/// A sync compares what a source holds with what is found on disk. What
/// this system has no way to keep would be found different every time, and
/// no sync could make the difference go away. So it is put here as it will
/// be found: on a system that is not Unix, of a file's permissions only
/// whether it may be written, and of a directory neither its permissions
/// nor its time. On Unix the tree is as it was.
///
/// # Errors
///
/// Returns an error only if `tree` is not a tree, which it is.
pub fn as_kept(tree: &Tree) -> Result<Tree> {
    as_kept_under(tree, tree.name())
}

/// The tree as this system keeps it, stored under a new root name.
///
/// # Errors
///
/// Returns an error if the destination name cannot be used.
pub fn as_kept_under(tree: &Tree, name: &str) -> Result<Tree> {
    let entries = tree
        .entries()
        .iter()
        .zip(tree.recorded())
        .map(|(entry, &recorded)| (entry.clone(), io::as_kept(&entry.kind, recorded)))
        .collect();
    Ok(Tree::new(name, tree.chunk_size(), entries)?)
}

/// The canonical form of `path`, and the name of what is there.
fn named(path: &Path) -> Result<(PathBuf, String)> {
    let root = path.canonicalize()?;
    let name = root
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| unsupported(&root, "has no usable UTF-8 name"))?
        .to_owned();
    Ok((root, name))
}

/// Total size in bytes of the regular files under `path`, for progress bars.
///
/// # Errors
///
/// Returns an error if the path, or anything under it, cannot be read.
pub fn measure(path: &Path) -> Result<u64> {
    let (sources, _) = walk(&path.canonicalize()?)?;
    Ok(sources.iter().map(|source| source.entry.size()).sum())
}

/// Everything at and under `root` that a manifest records, the root first
/// and the rest in path order, and the number of things left out.
fn walk(root: &Path) -> Result<(Vec<Source>, usize)> {
    let found = std::fs::metadata(root)?;
    let kind = if found.is_dir() {
        EntryKind::Directory
    } else if found.is_file() {
        EntryKind::File { size: found.len() }
    } else {
        return Err(unsupported(root, "is not a regular file or directory"));
    };
    let mut sources = vec![Source::new(root.to_owned(), String::new(), kind, &found)];
    let mut skipped = 0;
    if !found.is_dir() {
        return Ok((sources, skipped));
    }

    let mut entries = WalkDir::new(root).min_depth(1).into_iter();
    while let Some(entry) = entries.next() {
        let entry = entry.map_err(std::io::Error::from)?;
        let file_type = entry.file_type();
        // What a sync took away from this folder is not part of what the
        // folder holds.
        if entry.depth() == 1 && entry.file_name() == TRASH {
            if file_type.is_dir() {
                entries.skip_current_dir();
            }
            continue;
        }
        // A part file is this program's own unfinished work, and no manifest
        // may name one.
        if entry.file_name().to_str().is_some_and(is_part_name) {
            skipped += 1;
            if file_type.is_dir() {
                entries.skip_current_dir();
            }
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(root)
            .map_err(|_| unsupported(entry.path(), "is outside the indexed directory"))?;
        let names: Vec<&str> = relative
            .iter()
            .map(|c| c.to_str())
            .collect::<Option<_>>()
            .ok_or_else(|| unsupported(entry.path(), "has a non-UTF-8 name"))?;
        let path = names.join("/");
        // Asked of the file system directly, not taken from the walk. On
        // Windows the walk hands back what the parent directory has on
        // record for the entry, which can be out of date for a moment after
        // the entry changes. A link is not followed: this describes the link
        // itself.
        let found = std::fs::symlink_metadata(entry.path())?;

        let kind = if file_type.is_dir() {
            EntryKind::Directory
        } else if file_type.is_file() {
            EntryKind::File { size: found.len() }
        } else if let Some(target) = link_target(entry.path(), &path) {
            EntryKind::Symlink { target }
        } else {
            skipped += 1;
            continue;
        };
        sources.push(Source::new(entry.path().to_owned(), path, kind, &found));
    }
    sources[1..].sort_by(|a, b| a.entry.path.cmp(&b.entry.path));
    Ok((sources, skipped))
}

/// Where the link at `on_disk` points, if it is a link whose target stays
/// inside the tree. `path` is the link's own path in the manifest.
fn link_target(on_disk: &Path, path: &str) -> Option<String> {
    let target = std::fs::read_link(on_disk).ok()?;
    let target = target.to_str()?;
    // Windows writes a relative target with its own separator.
    #[cfg(windows)]
    let target = target.replace('\\', "/");
    is_safe_target(target.as_bytes(), depth(path.as_bytes())).then(|| target.to_owned())
}

fn hash_file(
    path: &Path,
    size: u64,
    chunk_size: u32,
    on_bytes: &(dyn Fn(u64) + Sync),
) -> Result<Vec<Hash>> {
    let chunk_size = u64::from(chunk_size);
    let file = File::open(path)?;
    (0..size.div_ceil(chunk_size))
        .into_par_iter()
        .map_init(Vec::new, |buf: &mut Vec<u8>, index| {
            let offset = index * chunk_size;
            let len = (size - offset).min(chunk_size);
            buf.resize(to_usize(len), 0);
            io::read_exact_at(&file, buf, offset)?;
            on_bytes(len);
            Ok(Hash::of(buf))
        })
        .collect()
}

fn unsupported(path: &Path, reason: &str) -> Error {
    Error::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!("{} {reason}", path.display()),
    ))
}
