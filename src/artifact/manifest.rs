use super::Hash;
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub const MIN_CHUNK_SIZE: u32 = 4 * 1024;
pub const MAX_CHUNK_SIZE: u32 = 16 * 1024 * 1024;
pub const DEFAULT_CHUNK_SIZE: u32 = 1024 * 1024;

/// Upper bound on an encoded manifest. At the default chunk size this covers
/// artifacts of roughly two petabytes.
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024 * 1024;

const MAX_COMPONENT_BYTES: usize = 255;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    /// A single file, stored as `<name>`.
    File,
    /// A directory tree, stored under `<name>/`.
    Directory,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    /// Path relative to the artifact root, with `/` separators.
    pub path: String,
    pub size: u64,
    pub executable: bool,
}

/// Describes an artifact: its files and the digest of every chunk.
///
/// Each file is chunked independently, so chunk `n` of a file always starts at
/// byte `n * chunk_size`. That lets a seeder serve straight from the original
/// files and lets a downloader write chunks in any order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub name: String,
    pub kind: Kind,
    pub chunk_size: u32,
    /// Sorted by path.
    pub files: Vec<FileEntry>,
    /// Chunk digests for every file, concatenated in file order.
    pub chunks: Vec<Hash>,
}

impl Manifest {
    /// Checks every invariant the rest of the program relies on. Manifests
    /// arrive from untrusted peers, so nothing is used before this passes.
    pub fn validate(&self) -> Result<()> {
        if !(MIN_CHUNK_SIZE..=MAX_CHUNK_SIZE).contains(&self.chunk_size) {
            return Err(invalid(format!(
                "chunk size {} is outside {MIN_CHUNK_SIZE}..={MAX_CHUNK_SIZE}",
                self.chunk_size
            )));
        }
        if !is_safe_component(&self.name) {
            return Err(invalid(format!("unsafe artifact name {:?}", self.name)));
        }

        match self.kind {
            Kind::File => {
                if self.files.len() != 1 || self.files[0].path != self.name {
                    return Err(invalid("a file artifact must list exactly its own file"));
                }
            }
            Kind::Directory => self.validate_tree()?,
        }

        let mut expected: u64 = 0;
        let mut total: u64 = 0;
        for file in &self.files {
            total = total
                .checked_add(file.size)
                .ok_or_else(|| invalid("total size overflows"))?;
            expected += file.size.div_ceil(u64::from(self.chunk_size));
        }
        if expected != self.chunks.len() as u64 {
            return Err(invalid(format!(
                "file sizes imply {expected} chunks but {} digests are listed",
                self.chunks.len()
            )));
        }
        Ok(())
    }

    fn validate_tree(&self) -> Result<()> {
        let mut directories: HashSet<&str> = HashSet::new();
        let mut previous: Option<&str> = None;
        for file in &self.files {
            let path = file.path.as_str();
            if !path.split('/').all(is_safe_component) {
                return Err(invalid(format!("unsafe path {path:?}")));
            }
            if previous.is_some_and(|p| p >= path) {
                return Err(invalid("file paths must be sorted and unique"));
            }
            previous = Some(path);
            for (i, _) in path.match_indices('/') {
                directories.insert(&path[..i]);
            }
        }
        if let Some(clash) = self
            .files
            .iter()
            .find(|f| directories.contains(f.path.as_str()))
        {
            return Err(invalid(format!(
                "{:?} is both a file and a directory",
                clash.path
            )));
        }
        Ok(())
    }
}

/// Where a chunk lives inside an artifact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChunkLocation {
    /// Index into `Manifest::files`.
    pub file: usize,
    pub offset: u64,
    pub len: u32,
}

/// A validated manifest together with its identifier and chunk layout.
#[derive(Debug)]
pub struct Artifact {
    id: Hash,
    manifest: Manifest,
    encoded: Vec<u8>,
    /// Global index of the first chunk of each file.
    first_chunk: Vec<u64>,
    total_size: u64,
}

const ID_CONTEXT: &str = "p2rent 2026 manifest v1";

impl Artifact {
    /// Builds an artifact from a locally produced manifest.
    pub fn new(manifest: Manifest) -> Result<Self> {
        manifest.validate()?;
        let encoded =
            postcard::to_allocvec(&manifest).map_err(|e| invalid(format!("cannot encode: {e}")))?;
        if encoded.len() > MAX_MANIFEST_BYTES {
            return Err(invalid(
                "artifact has too many chunks; use a larger chunk size",
            ));
        }
        Ok(Self::assemble(manifest, encoded))
    }

    /// Decodes manifest bytes received from a peer, accepting them only if
    /// they hash to `expected` and describe a valid artifact.
    pub fn decode(expected: Hash, encoded: Vec<u8>) -> Result<Self> {
        if encoded.len() > MAX_MANIFEST_BYTES {
            return Err(invalid("manifest exceeds the size limit"));
        }
        if Hash::derive(ID_CONTEXT, &encoded) != expected {
            return Err(invalid("manifest does not match the artifact id"));
        }
        let manifest: Manifest =
            postcard::from_bytes(&encoded).map_err(|e| invalid(format!("cannot decode: {e}")))?;
        manifest.validate()?;
        Ok(Self::assemble(manifest, encoded))
    }

    fn assemble(manifest: Manifest, encoded: Vec<u8>) -> Self {
        let chunk_size = u64::from(manifest.chunk_size);
        let mut first_chunk = Vec::with_capacity(manifest.files.len());
        let mut next = 0u64;
        let mut total_size = 0u64;
        for file in &manifest.files {
            first_chunk.push(next);
            next += file.size.div_ceil(chunk_size);
            total_size += file.size;
        }
        Self {
            id: Hash::derive(ID_CONTEXT, &encoded),
            manifest,
            encoded,
            first_chunk,
            total_size,
        }
    }

    pub fn id(&self) -> Hash {
        self.id
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// The exact bytes that hash to [`Artifact::id`], as sent to peers.
    pub fn encoded(&self) -> &[u8] {
        &self.encoded
    }

    pub fn total_size(&self) -> u64 {
        self.total_size
    }

    pub fn chunk_count(&self) -> u64 {
        self.manifest.chunks.len() as u64
    }

    pub fn chunk_hash(&self, index: u64) -> Option<Hash> {
        self.manifest
            .chunks
            .get(usize::try_from(index).ok()?)
            .copied()
    }

    /// Global chunk indices belonging to one file.
    pub fn file_chunks(&self, file: usize) -> std::ops::Range<u64> {
        let start = self.first_chunk[file];
        let count = self.manifest.files[file]
            .size
            .div_ceil(u64::from(self.manifest.chunk_size));
        start..start + count
    }

    pub fn locate(&self, index: u64) -> Option<ChunkLocation> {
        if index >= self.chunk_count() {
            return None;
        }
        // Empty files own no chunks and share a start index with the file
        // after them, so take the last file starting at or before `index`.
        let file = self.first_chunk.partition_point(|&start| start <= index) - 1;
        let chunk_size = u64::from(self.manifest.chunk_size);
        let offset = (index - self.first_chunk[file]) * chunk_size;
        let len = (self.manifest.files[file].size - offset).min(chunk_size) as u32;
        Some(ChunkLocation { file, offset, len })
    }
}

/// A path component that cannot escape the destination directory or be
/// reinterpreted on any supported platform.
pub fn is_safe_component(component: &str) -> bool {
    !component.is_empty()
        && component.len() <= MAX_COMPONENT_BYTES
        && component != "."
        && component != ".."
        && !component
            .chars()
            .any(|c| matches!(c, '/' | '\\' | ':' | '\0'))
}

fn invalid(reason: impl Into<String>) -> Error {
    Error::InvalidManifest(reason.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, size: u64) -> FileEntry {
        FileEntry {
            path: path.into(),
            size,
            executable: false,
        }
    }

    fn tree(files: Vec<FileEntry>) -> Manifest {
        let chunk_size = MIN_CHUNK_SIZE;
        let chunks = files
            .iter()
            .map(|f| f.size.div_ceil(u64::from(chunk_size)))
            .sum::<u64>();
        Manifest {
            name: "data".into(),
            kind: Kind::Directory,
            chunk_size,
            files,
            chunks: (0..chunks).map(|i| Hash::of(&i.to_le_bytes())).collect(),
        }
    }

    #[test]
    fn accepts_a_well_formed_tree() {
        let manifest = tree(vec![entry("a.bin", 10_000), entry("sub/b.bin", 1)]);
        manifest.validate().unwrap();
    }

    #[test]
    fn rejects_paths_that_escape_the_root() {
        for path in [
            "../x",
            "/etc/passwd",
            "a/../../x",
            "a//b",
            "a\\b",
            "c:x",
            ".",
        ] {
            let manifest = tree(vec![entry(path, 1)]);
            assert!(manifest.validate().is_err(), "{path:?} was accepted");
        }
    }

    #[test]
    fn rejects_unsorted_duplicate_or_clashing_paths() {
        assert!(tree(vec![entry("b", 1), entry("a", 1)]).validate().is_err());
        assert!(tree(vec![entry("a", 1), entry("a", 1)]).validate().is_err());
        assert!(
            tree(vec![entry("a", 1), entry("a-b", 1), entry("a/b", 1)])
                .validate()
                .is_err()
        );
    }

    #[test]
    fn rejects_wrong_chunk_count_and_chunk_size() {
        let mut manifest = tree(vec![entry("a", 10_000)]);
        manifest.chunks.pop();
        assert!(manifest.validate().is_err());

        let mut manifest = tree(vec![entry("a", 1)]);
        manifest.chunk_size = 1;
        assert!(manifest.validate().is_err());
    }

    #[test]
    fn rejects_unsafe_artifact_name() {
        let mut manifest = tree(vec![entry("a", 1)]);
        manifest.name = "..".into();
        assert!(manifest.validate().is_err());
    }

    #[test]
    fn locates_chunks_across_files_including_empty_ones() {
        let cs = u64::from(MIN_CHUNK_SIZE);
        let artifact = Artifact::new(tree(vec![
            entry("a", cs * 2 + 5),
            entry("b", 0),
            entry("c", cs),
            entry("d", 0),
        ]))
        .unwrap();

        assert_eq!(artifact.chunk_count(), 4);
        assert_eq!(artifact.total_size(), cs * 3 + 5);
        let at = |i| artifact.locate(i).unwrap();
        assert_eq!((at(0).file, at(0).offset, at(0).len as u64), (0, 0, cs));
        assert_eq!((at(2).file, at(2).offset, at(2).len), (0, cs * 2, 5));
        assert_eq!((at(3).file, at(3).offset, at(3).len as u64), (2, 0, cs));
        assert!(artifact.locate(4).is_none());
        assert_eq!(artifact.file_chunks(1), 3..3);
        assert_eq!(artifact.file_chunks(2), 3..4);
    }

    #[test]
    fn decode_requires_matching_id() {
        let artifact = Artifact::new(tree(vec![entry("a", 1)])).unwrap();
        let bytes = artifact.encoded().to_vec();

        let decoded = Artifact::decode(artifact.id(), bytes.clone()).unwrap();
        assert_eq!(decoded.manifest(), artifact.manifest());

        assert!(Artifact::decode(Hash::of(b"other"), bytes.clone()).is_err());
        let mut tampered = bytes;
        *tampered.last_mut().unwrap() ^= 1;
        assert!(Artifact::decode(artifact.id(), tampered).is_err());
    }
}
