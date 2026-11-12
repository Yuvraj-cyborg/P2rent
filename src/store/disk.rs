use super::io;
use crate::artifact::{Artifact, Bitfield, Hash, Kind};
use rayon::prelude::*;
use std::fs::{File, OpenOptions};
use std::io::{Error, ErrorKind, Result};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// An artifact's files on disk, addressed by chunk index.
///
/// Seeders point this at the original files; downloaders point it at the
/// destination and fill it in. Either way the bytes live in exactly one place.
#[derive(Debug)]
pub struct DiskStore {
    artifact: Arc<Artifact>,
    root: PathBuf,
}

impl DiskStore {
    /// `root` is the file itself for a file artifact, or the directory that
    /// holds the tree for a directory artifact.
    pub fn new(artifact: Arc<Artifact>, root: PathBuf) -> Self {
        Self { artifact, root }
    }

    /// A store that places the artifact, under its own name, inside `dir`.
    pub fn inside(artifact: Arc<Artifact>, dir: &Path) -> Self {
        let root = dir.join(&artifact.manifest().name);
        Self::new(artifact, root)
    }

    pub fn artifact(&self) -> &Arc<Artifact> {
        &self.artifact
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn file_path(&self, file: usize) -> PathBuf {
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

    pub fn read_chunk(&self, index: u64) -> Result<Vec<u8>> {
        let location = self.artifact.locate(index).ok_or_else(out_of_range)?;
        let file = File::open(self.file_path(location.file))?;
        let mut buf = vec![0; location.len as usize];
        io::read_exact_at(&file, &mut buf, location.offset)?;
        Ok(buf)
    }

    /// Writes one chunk in place. The file must have been created by
    /// [`DiskStore::allocate`].
    pub fn write_chunk(&self, index: u64, data: &[u8]) -> Result<()> {
        let location = self.artifact.locate(index).ok_or_else(out_of_range)?;
        if data.len() != location.len as usize {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "chunk has wrong length",
            ));
        }
        let file = OpenOptions::new()
            .write(true)
            .open(self.file_path(location.file))?;
        io::write_all_at(&file, data, location.offset)
    }

    /// Creates every directory and file at its final size. Existing files are
    /// kept, so data from an interrupted download survives.
    pub fn allocate(&self) -> Result<()> {
        let manifest = self.artifact.manifest();
        for (index, entry) in manifest.files.iter().enumerate() {
            let path = self.file_path(index);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .open(&path)?;
            if file.metadata()?.len() != entry.size {
                file.set_len(entry.size)?;
            }
        }
        if manifest.kind == Kind::Directory {
            std::fs::create_dir_all(&self.root)?;
        }
        Ok(())
    }

    /// Hashes whatever is already on disk and reports which chunks are valid.
    ///
    /// Missing files cost nothing. `on_bytes` is called from worker threads
    /// with the size of each chunk examined.
    pub fn scan(&self, on_bytes: &(dyn Fn(u64) + Sync)) -> Result<Bitfield> {
        let files = self.artifact.manifest().files.len();
        let valid: Vec<Vec<u64>> = (0..files)
            .into_par_iter()
            .map(|file| self.scan_file(file, on_bytes))
            .collect::<Result<_>>()?;

        let mut have = Bitfield::new(self.artifact.chunk_count());
        for index in valid.into_iter().flatten() {
            have.set(index);
        }
        Ok(have)
    }

    fn scan_file(&self, file_index: usize, on_bytes: &(dyn Fn(u64) + Sync)) -> Result<Vec<u64>> {
        let chunks = self.artifact.file_chunks(file_index);
        let file = match File::open(self.file_path(file_index)) {
            Ok(file) => file,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(Error::new(
                ErrorKind::AlreadyExists,
                format!(
                    "{} exists and is not a regular file",
                    self.file_path(file_index).display()
                ),
            ));
        }
        let on_disk = metadata.len();

        let valid = chunks
            .into_par_iter()
            .map_init(Vec::new, |buf: &mut Vec<u8>, index| {
                let location = self.artifact.locate(index).ok_or_else(out_of_range)?;
                on_bytes(u64::from(location.len));
                if location.offset + u64::from(location.len) > on_disk {
                    return Ok(None);
                }
                buf.resize(location.len as usize, 0);
                io::read_exact_at(&file, buf, location.offset)?;
                let matches = Some(Hash::of(buf)) == self.artifact.chunk_hash(index);
                Ok(matches.then_some(index))
            })
            .collect::<Result<Vec<Option<u64>>>>()?;
        Ok(valid.into_iter().flatten().collect())
    }

    /// Flushes every file to stable storage and restores execute bits.
    pub fn finalize(&self) -> Result<()> {
        for (index, entry) in self.artifact.manifest().files.iter().enumerate() {
            let file = OpenOptions::new().write(true).open(self.file_path(index))?;
            if entry.executable {
                io::make_executable(&file)?;
            }
            file.sync_all()?;
        }
        Ok(())
    }
}

fn out_of_range() -> Error {
    Error::new(ErrorKind::InvalidInput, "chunk index out of range")
}
