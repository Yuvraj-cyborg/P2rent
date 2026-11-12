use super::io;
use crate::artifact::{Artifact, FileEntry, Hash, Kind, Manifest};
use crate::error::{Error, Result};
use rayon::prelude::*;
use std::fs::File;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// The result of hashing a local file or directory.
#[derive(Debug)]
pub struct Indexed {
    pub artifact: Artifact,
    /// Canonical path of the file or directory that was indexed.
    pub root: PathBuf,
    /// Entries that are neither regular files nor directories (symlinks,
    /// sockets, devices) and were left out.
    pub skipped: usize,
}

struct Source {
    path: PathBuf,
    entry: FileEntry,
}

/// Hashes `path` into an artifact without copying any data.
///
/// `on_bytes` is called from worker threads with the size of each chunk as it
/// is hashed.
pub fn index(path: &Path, chunk_size: u32, on_bytes: &(dyn Fn(u64) + Sync)) -> Result<Indexed> {
    let root = path.canonicalize()?;
    let name = root
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| unsupported(&root, "has no usable UTF-8 name"))?
        .to_owned();

    let metadata = std::fs::metadata(&root)?;
    let (kind, sources, skipped) = if metadata.is_dir() {
        let (sources, skipped) = walk(&root)?;
        (Kind::Directory, sources, skipped)
    } else if metadata.is_file() {
        let entry = FileEntry {
            path: name.clone(),
            size: metadata.len(),
            executable: io::is_executable(&metadata),
        };
        let path = root.clone();
        (Kind::File, vec![Source { path, entry }], 0)
    } else {
        return Err(unsupported(&root, "is not a regular file or directory"));
    };

    let per_file: Vec<Vec<Hash>> = sources
        .par_iter()
        .map(|source| hash_file(&source.path, source.entry.size, chunk_size, on_bytes))
        .collect::<Result<_>>()?;

    let manifest = Manifest {
        name,
        kind,
        chunk_size,
        files: sources.into_iter().map(|s| s.entry).collect(),
        chunks: per_file.into_iter().flatten().collect(),
    };
    Ok(Indexed {
        artifact: Artifact::new(manifest)?,
        root,
        skipped,
    })
}

/// Total size in bytes of the regular files under `path`, for progress bars.
pub fn measure(path: &Path) -> Result<u64> {
    let metadata = std::fs::metadata(path)?;
    if !metadata.is_dir() {
        return Ok(metadata.len());
    }
    let (sources, _) = walk(&path.canonicalize()?)?;
    Ok(sources.iter().map(|s| s.entry.size).sum())
}

fn walk(root: &Path) -> Result<(Vec<Source>, usize)> {
    let mut sources = Vec::new();
    let mut skipped = 0;
    for entry in WalkDir::new(root).min_depth(1) {
        let entry = entry.map_err(std::io::Error::from)?;
        let file_type = entry.file_type();
        if file_type.is_dir() {
            continue;
        }
        if !file_type.is_file() {
            skipped += 1;
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(root)
            .map_err(|_| unsupported(entry.path(), "is outside the indexed directory"))?;
        let components: Vec<&str> = relative
            .iter()
            .map(|c| c.to_str())
            .collect::<Option<_>>()
            .ok_or_else(|| unsupported(entry.path(), "has a non-UTF-8 name"))?;
        let metadata = entry.metadata().map_err(std::io::Error::from)?;
        sources.push(Source {
            path: entry.path().to_owned(),
            entry: FileEntry {
                path: components.join("/"),
                size: metadata.len(),
                executable: io::is_executable(&metadata),
            },
        });
    }
    sources.sort_by(|a, b| a.entry.path.cmp(&b.entry.path));
    Ok((sources, skipped))
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
            buf.resize(len as usize, 0);
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
