use super::Hash;
use super::layout::{ChunkLocation, Layout};
use crate::error::{Error, Result};
use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

/// Smallest chunk size a manifest may use.
pub const MIN_CHUNK_SIZE: u32 = 4 * 1024;
/// Largest chunk size a manifest may use.
pub const MAX_CHUNK_SIZE: u32 = 16 * 1024 * 1024;
/// Chunk size used when none is requested.
pub const DEFAULT_CHUNK_SIZE: u32 = 1024 * 1024;

/// Upper bound on an encoded manifest. At the default chunk size this covers
/// artifacts of roughly two petabytes.
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024 * 1024;

const MAX_COMPONENT_BYTES: usize = 255;

/// Whether an artifact is a single file or a directory tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    /// A single file, stored as `<name>`.
    File,
    /// A directory tree, stored under `<name>/`.
    Directory,
}

/// One file in an artifact.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    /// Path relative to the artifact root, with `/` separators.
    pub path: String,
    /// Size in bytes.
    pub size: u64,
    /// Whether any execute permission bit is set.
    pub executable: bool,
}

/// Describes an artifact: its files and the digest of every chunk.
///
/// Each file is chunked independently, so chunk `n` of a file always starts at
/// byte `n * chunk_size`. That lets a seeder serve straight from the original
/// files and lets a downloader write chunks in any order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// File or directory name the artifact is stored under.
    pub name: String,
    /// Whether this is a single file or a directory tree.
    pub kind: Kind,
    /// Size of every chunk except, possibly, the last one of each file.
    pub chunk_size: u32,
    /// Sorted by path.
    pub files: Vec<FileEntry>,
    /// Chunk digests for every file, concatenated in file order.
    pub chunks: Vec<Hash>,
}

impl Manifest {
    /// Checks every invariant the rest of the program relies on. Manifests
    /// arrive from untrusted peers, so nothing is used before this passes.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidManifest`] naming the first invariant that fails.
    pub fn validate(&self) -> Result<()> {
        self.checked_layout().map(drop)
    }

    /// Validates the manifest and returns the chunk layout it describes.
    fn checked_layout(&self) -> Result<Layout> {
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

        let sizes: Vec<u64> = self.files.iter().map(|file| file.size).collect();
        let layout =
            Layout::new(&sizes, self.chunk_size).ok_or_else(|| invalid("total size overflows"))?;
        if layout.chunk_count() != self.chunks.len() as u64 {
            return Err(invalid(format!(
                "file sizes imply {} chunks but {} digests are listed",
                layout.chunk_count(),
                self.chunks.len()
            )));
        }
        Ok(layout)
    }

    fn validate_tree(&self) -> Result<()> {
        let mut directories: BTreeSet<&str> = BTreeSet::new();
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

/// A validated manifest together with its identifier and chunk layout.
#[derive(Debug)]
pub struct Artifact {
    id: Hash,
    manifest: Manifest,
    encoded: Vec<u8>,
    layout: Layout,
}

const ID_CONTEXT: &str = "p2rent 2026 manifest v1";

impl Artifact {
    /// Builds an artifact from a locally produced manifest.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidManifest`] if the manifest is invalid or too large to encode.
    pub fn new(manifest: Manifest) -> Result<Self> {
        let layout = manifest.checked_layout()?;
        let encoded =
            postcard::to_allocvec(&manifest).map_err(|e| invalid(format!("cannot encode: {e}")))?;
        if encoded.len() > MAX_MANIFEST_BYTES {
            return Err(invalid(
                "artifact has too many chunks; use a larger chunk size",
            ));
        }
        Ok(Self::assemble(manifest, encoded, layout))
    }

    /// Decodes manifest bytes received from a peer, accepting them only if
    /// they hash to `expected` and describe a valid artifact.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidManifest`] if the bytes exceed the size limit, do not hash to
    /// `expected`, cannot be decoded or fail validation.
    pub fn decode(expected: Hash, encoded: Vec<u8>) -> Result<Self> {
        if encoded.len() > MAX_MANIFEST_BYTES {
            return Err(invalid("manifest exceeds the size limit"));
        }
        if Hash::derive(ID_CONTEXT, &encoded) != expected {
            return Err(invalid("manifest does not match the artifact id"));
        }
        let manifest: Manifest =
            postcard::from_bytes(&encoded).map_err(|e| invalid(format!("cannot decode: {e}")))?;
        let layout = manifest.checked_layout()?;
        Ok(Self::assemble(manifest, encoded, layout))
    }

    fn assemble(manifest: Manifest, encoded: Vec<u8>, layout: Layout) -> Self {
        Self {
            id: Hash::derive(ID_CONTEXT, &encoded),
            manifest,
            encoded,
            layout,
        }
    }

    /// The artifact id: the hash of the encoded manifest.
    pub fn id(&self) -> Hash {
        self.id
    }

    /// The validated manifest.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// The exact bytes that hash to [`Artifact::id`], as sent to peers.
    pub fn encoded(&self) -> &[u8] {
        &self.encoded
    }

    /// Sum of all file sizes, in bytes.
    pub fn total_size(&self) -> u64 {
        self.layout.total_size()
    }

    /// Number of chunks across all files.
    pub fn chunk_count(&self) -> u64 {
        self.manifest.chunks.len() as u64
    }

    /// Expected digest of chunk `index`, or `None` if it is out of range.
    pub fn chunk_hash(&self, index: u64) -> Option<Hash> {
        self.manifest
            .chunks
            .get(usize::try_from(index).ok()?)
            .copied()
    }

    /// Global chunk indices belonging to one file.
    pub fn file_chunks(&self, file: usize) -> core::ops::Range<u64> {
        let (start, end) = self.layout.file_chunks(file);
        start..end
    }

    /// Where chunk `index` lives, or `None` if it is out of range.
    pub fn locate(&self, index: u64) -> Option<ChunkLocation> {
        self.layout.locate(index)
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

    /// These limits decide which manifests every node accepts, so a change
    /// to any of them is a change to the protocol.
    #[test]
    fn the_limits_are_what_the_protocol_says() {
        assert_eq!(MIN_CHUNK_SIZE, 4096);
        assert_eq!(DEFAULT_CHUNK_SIZE, 1_048_576);
        assert_eq!(MAX_CHUNK_SIZE, 16_777_216);
        assert_eq!(MAX_MANIFEST_BYTES, 67_108_864);
    }

    #[test]
    fn a_file_artifact_lists_exactly_its_own_file() {
        let file = |name: &str, files: Vec<FileEntry>| Manifest {
            name: name.into(),
            kind: Kind::File,
            ..tree(files)
        };
        file("a", vec![entry("a", 1)]).validate().unwrap();
        assert!(file("a", vec![entry("b", 1)]).validate().is_err());
        assert!(
            file("a", vec![entry("a", 1), entry("b", 1)])
                .validate()
                .is_err()
        );
        assert!(file("a", vec![]).validate().is_err());
    }

    #[test]
    fn chunk_hashes_come_from_the_manifest() {
        let manifest = tree(vec![entry("a", 10_000)]);
        let chunks = manifest.chunks.clone();
        let artifact = Artifact::new(manifest).unwrap();
        assert_eq!(artifact.chunk_hash(0), Some(chunks[0]));
        assert_eq!(artifact.chunk_hash(2), Some(chunks[2]));
        assert_eq!(artifact.chunk_hash(3), None);
    }

    /// The size check comes first, and allows a manifest of exactly the
    /// limit.
    #[test]
    #[cfg_attr(miri, ignore = "hashes 64 MiB, which takes hours when interpreted")]
    fn decode_enforces_the_size_limit_at_its_edge() {
        let too_large = |bytes: Vec<u8>| {
            let reason = Artifact::decode(Hash::of(b"any"), bytes)
                .unwrap_err()
                .to_string();
            reason.contains("size limit")
        };
        assert!(too_large(vec![0; MAX_MANIFEST_BYTES + 1]));
        assert!(!too_large(vec![0; MAX_MANIFEST_BYTES]));
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
        assert_eq!((at(0).file, at(0).offset, u64::from(at(0).len)), (0, 0, cs));
        assert_eq!((at(2).file, at(2).offset, at(2).len), (0, cs * 2, 5));
        assert_eq!((at(3).file, at(3).offset, u64::from(at(3).len)), (2, 0, cs));
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
