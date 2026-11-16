use super::Hash;
use super::layout::{ChunkLocation, Layout};
use super::path;
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

/// The manifest format this build writes and reads. It is the first byte of
/// every encoded manifest, and part of how each id is derived, so a manifest
/// in a format this build does not know is refused instead of misread.
pub const FORMAT_VERSION: u8 = 1;

/// What is added to a file's name while it is being written. No name in a
/// manifest may end with it, in any letter case, so a part file can never be
/// mistaken for a file of the artifact or collide with one.
pub const PART_SUFFIX: &str = ".mux-part";

pub use path::{MAX_NAME_BYTES, MAX_TARGET_BYTES};

/// Every permission bit a manifest may record: read, write and execute for
/// the owner, the group and everyone else.
pub const PERMISSION_BITS: u16 = 0o777;

/// What an entry of an artifact is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntryKind {
    /// A regular file.
    File {
        /// Its size in bytes.
        size: u64,
    },
    /// A directory. Every directory is listed, so that empty ones survive.
    Directory,
    /// A symbolic link.
    Symlink {
        /// Where it points: a relative path that stays inside the artifact.
        target: String,
    },
}

/// One file, directory or link of an artifact.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Path relative to the artifact's root, with `/` separators. The root
    /// itself has the empty path.
    pub path: String,
    /// What is at that path.
    pub kind: EntryKind,
}

impl Entry {
    /// The number of bytes the entry holds: a file's size, and nothing for a
    /// directory or a link.
    pub fn size(&self) -> u64 {
        match self.kind {
            EntryKind::File { size } => size,
            EntryKind::Directory | EntryKind::Symlink { .. } => 0,
        }
    }

    /// Whether the entry is a regular file.
    pub fn is_file(&self) -> bool {
        matches!(self.kind, EntryKind::File { .. })
    }
}

/// What an artifact is: its tree of entries and the digest of every chunk.
///
/// Each file is chunked independently, so chunk `n` of a file always starts at
/// byte `n * chunk_size`. That lets a seeder serve straight from the original
/// files and lets a downloader write chunks in any order.
///
/// Nothing here depends on when a file was last changed or who may read it.
/// Two trees with the same bytes at the same paths have the same content.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Content {
    /// Size of every chunk except, possibly, the last one of each file.
    pub chunk_size: u32,
    /// The root first, then everything under it, sorted by path.
    pub entries: Vec<Entry>,
    /// Chunk digests of every file, concatenated in entry order.
    pub chunks: Vec<Hash>,
}

/// A point in time, counted from 1970-01-01 00:00:00 UTC.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Timestamp {
    /// Whole seconds. Negative for times before 1970.
    pub seconds: i64,
    /// Nanoseconds after that second, below one thousand million.
    pub nanos: u32,
}

/// What is recorded about an entry besides its content.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryMetadata {
    /// Permission bits as Unix has them, within [`PERMISSION_BITS`].
    pub mode: u16,
    /// When the entry was last modified, if that is known.
    pub modified: Option<Timestamp>,
}

/// Names, permissions and times: everything about an artifact that is not
/// its content.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Metadata {
    /// The name the root is stored under.
    pub name: String,
    /// One for each entry of the content, in the same order.
    pub entries: Vec<EntryMetadata>,
}

/// Describes an artifact: what it is, and the names, permissions and times
/// that go with it.
///
/// The two halves are identified apart. The content id comes from
/// [`Manifest::content`] alone and names the swarm: any node that holds those
/// bytes can supply them. The artifact id comes from the whole manifest, so a
/// ticket also pins the metadata.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// The tree of entries and the digest of every chunk.
    pub content: Content,
    /// Names, permissions and times.
    pub metadata: Metadata,
}

impl Manifest {
    /// Puts a manifest together from entries, each with what is recorded
    /// about it, and the digests of their chunks. The entries must already
    /// be in order, the root first. Nothing is checked here: see
    /// [`Manifest::validate`].
    pub fn new(
        name: impl Into<String>,
        chunk_size: u32,
        entries: Vec<(Entry, EntryMetadata)>,
        chunks: Vec<Hash>,
    ) -> Self {
        let (entries, metadata) = entries.into_iter().unzip();
        Self {
            content: Content {
                chunk_size,
                entries,
                chunks,
            },
            metadata: Metadata {
                name: name.into(),
                entries: metadata,
            },
        }
    }

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
        let content = &self.content;
        if !(MIN_CHUNK_SIZE..=MAX_CHUNK_SIZE).contains(&content.chunk_size) {
            return Err(invalid(format!(
                "chunk size {} is outside {MIN_CHUNK_SIZE}..={MAX_CHUNK_SIZE}",
                content.chunk_size
            )));
        }
        if !is_safe_component(&self.metadata.name) {
            return Err(invalid(format!(
                "unsafe artifact name {:?}",
                self.metadata.name
            )));
        }
        self.validate_tree()?;
        self.validate_metadata()?;

        let sizes: Vec<u64> = content.entries.iter().map(Entry::size).collect();
        let layout = Layout::new(&sizes, content.chunk_size)
            .ok_or_else(|| invalid("total size overflows"))?;
        if layout.chunk_count() != content.chunks.len() as u64 {
            return Err(invalid(format!(
                "file sizes imply {} chunks but {} digests are listed",
                layout.chunk_count(),
                content.chunks.len()
            )));
        }
        Ok(layout)
    }

    /// Checks the shape of the tree: a root, then safe paths in order, each
    /// inside a directory of the artifact. Because a path's parent has to be
    /// a listed directory, nothing can lie under a file or behind a link.
    fn validate_tree(&self) -> Result<()> {
        let Some((root, rest)) = self.content.entries.split_first() else {
            return Err(invalid("an artifact has at least its root"));
        };
        if !root.path.is_empty() {
            return Err(invalid("the first entry is the root, with an empty path"));
        }
        let mut directories: BTreeSet<&str> = BTreeSet::new();
        match root.kind {
            EntryKind::File { .. } if rest.is_empty() => {}
            EntryKind::File { .. } => {
                return Err(invalid("a file artifact holds exactly one file"));
            }
            EntryKind::Directory => {
                directories.insert("");
            }
            EntryKind::Symlink { .. } => return Err(invalid("the root cannot be a link")),
        }

        let mut previous = "";
        for entry in rest {
            let path = entry.path.as_str();
            if !path::is_safe_path(path.as_bytes()) {
                return Err(invalid(format!("unsafe path {path:?}")));
            }
            if !path::precedes(previous.as_bytes(), path.as_bytes()) {
                return Err(invalid("paths must be sorted and unique"));
            }
            previous = path;

            let inside = path
                .get(..path::parent_len(path.as_bytes()))
                .is_some_and(|parent| directories.contains(parent));
            if !inside {
                return Err(invalid(format!(
                    "{path:?} is not inside a directory of the artifact"
                )));
            }
            match &entry.kind {
                EntryKind::File { .. } => {}
                EntryKind::Directory => {
                    directories.insert(path);
                }
                EntryKind::Symlink { target } => {
                    if !path::is_safe_target(target.as_bytes(), path::depth(path.as_bytes())) {
                        return Err(invalid(format!(
                            "link {path:?} points outside the artifact, at {target:?}"
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    fn validate_metadata(&self) -> Result<()> {
        if self.metadata.entries.len() != self.content.entries.len() {
            return Err(invalid(
                "permissions and times are listed for a different number of entries",
            ));
        }
        for metadata in &self.metadata.entries {
            if metadata.mode & !PERMISSION_BITS != 0 {
                return Err(invalid(format!(
                    "mode {:o} has bits beyond the permissions",
                    metadata.mode
                )));
            }
            if metadata
                .modified
                .is_some_and(|time| time.nanos >= 1_000_000_000)
            {
                return Err(invalid(
                    "a time has a second's worth of nanoseconds or more",
                ));
            }
        }
        Ok(())
    }
}

/// A validated manifest together with its identifiers and chunk layout.
#[derive(Debug)]
pub struct Artifact {
    id: Hash,
    content_id: Hash,
    manifest: Manifest,
    encoded: Vec<u8>,
    layout: Layout,
}

// Each id is derived under a context that names the format, so an id of one
// kind or one format can never be taken for another.
const ID_CONTEXT: &str = "mux 2026 manifest v1";
const CONTENT_CONTEXT: &str = "mux 2026 content v1";

/// The bytes of an encoded manifest before the content: the format version,
/// then the content's length as eight little-endian bytes.
const HEADER_BYTES: usize = 9;

impl Artifact {
    /// Builds an artifact from a locally produced manifest.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidManifest`] if the manifest is invalid or too large to encode.
    pub fn new(manifest: Manifest) -> Result<Self> {
        let layout = manifest.checked_layout()?;
        let cannot_encode = |e| invalid(format!("cannot encode: {e}"));
        let content = postcard::to_allocvec(&manifest.content).map_err(cannot_encode)?;

        let mut encoded = Vec::with_capacity(HEADER_BYTES + content.len());
        encoded.push(FORMAT_VERSION);
        encoded.extend_from_slice(&(content.len() as u64).to_le_bytes());
        encoded.extend_from_slice(&content);
        let encoded = postcard::to_extend(&manifest.metadata, encoded).map_err(cannot_encode)?;
        if encoded.len() > MAX_MANIFEST_BYTES {
            return Err(invalid(
                "artifact has too many chunks; use a larger chunk size",
            ));
        }
        let (content_id, id) = ids(&content, &encoded[HEADER_BYTES + content.len()..]);
        Ok(Self {
            id,
            content_id,
            manifest,
            encoded,
            layout,
        })
    }

    /// Decodes manifest bytes received from a peer, accepting them only if
    /// they are in a format this build reads, hash to `expected`, and
    /// describe a valid artifact.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidManifest`] if the bytes exceed the size limit, are in another
    /// format, do not hash to `expected`, cannot be decoded or fail validation.
    pub fn decode(expected: Hash, encoded: Vec<u8>) -> Result<Self> {
        if encoded.len() > MAX_MANIFEST_BYTES {
            return Err(invalid("manifest exceeds the size limit"));
        }
        match encoded.first() {
            Some(&FORMAT_VERSION) => {}
            Some(&version) => {
                return Err(invalid(format!(
                    "format version {version} is not supported: this build reads version \
                     {FORMAT_VERSION}"
                )));
            }
            None => return Err(invalid("manifest is empty")),
        }
        let (content, metadata) = encoded
            .get(1..HEADER_BYTES)
            .and_then(|length| length.try_into().ok())
            .and_then(|length| usize::try_from(u64::from_le_bytes(length)).ok())
            .and_then(|length| encoded.get(HEADER_BYTES..)?.split_at_checked(length))
            .ok_or_else(|| invalid("manifest is cut short"))?;

        // Nothing is decoded until the bytes are known to be the ones asked
        // for.
        let (content_id, id) = ids(content, metadata);
        if id != expected {
            return Err(invalid("manifest does not match the artifact id"));
        }
        let cannot_decode = |e| invalid(format!("cannot decode: {e}"));
        let manifest = Manifest {
            content: whole(postcard::take_from_bytes(content).map_err(cannot_decode)?)?,
            metadata: whole(postcard::take_from_bytes(metadata).map_err(cannot_decode)?)?,
        };
        let layout = manifest.checked_layout()?;
        Ok(Self {
            id,
            content_id,
            manifest,
            encoded,
            layout,
        })
    }

    /// The artifact id: the hash of the content id and the metadata. A
    /// ticket carries it, so it pins the content and the metadata both.
    pub fn id(&self) -> Hash {
        self.id
    }

    /// The content id: the hash of the content alone. It names the swarm.
    /// Two artifacts that differ only in names, permissions or times share
    /// it, and so share their chunks.
    pub fn content_id(&self) -> Hash {
        self.content_id
    }

    /// The validated manifest.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// The exact bytes that hash to [`Artifact::id`], as sent to peers.
    pub fn encoded(&self) -> &[u8] {
        &self.encoded
    }

    /// The name the root is stored under.
    pub fn name(&self) -> &str {
        &self.manifest.metadata.name
    }

    /// Every file, directory and link, the root first.
    pub fn entries(&self) -> &[Entry] {
        &self.manifest.content.entries
    }

    /// What is recorded about entry `entry` besides its content, or `None`
    /// if there is no such entry.
    pub fn metadata(&self, entry: usize) -> Option<EntryMetadata> {
        self.manifest.metadata.entries.get(entry).copied()
    }

    /// Whether the artifact is a single file and not a directory tree.
    pub fn is_file(&self) -> bool {
        self.entries().first().is_some_and(Entry::is_file)
    }

    /// Sum of all file sizes, in bytes.
    pub fn total_size(&self) -> u64 {
        self.layout.total_size()
    }

    /// Number of chunks across all files.
    pub fn chunk_count(&self) -> u64 {
        self.manifest.content.chunks.len() as u64
    }

    /// Expected digest of chunk `index`, or `None` if it is out of range.
    pub fn chunk_hash(&self, index: u64) -> Option<Hash> {
        self.manifest
            .content
            .chunks
            .get(usize::try_from(index).ok()?)
            .copied()
    }

    /// Global chunk indices belonging to one entry. Empty for a directory or
    /// a link.
    pub fn file_chunks(&self, entry: usize) -> core::ops::Range<u64> {
        let (start, end) = self.layout.file_chunks(entry);
        start..end
    }

    /// Where chunk `index` lives, or `None` if it is out of range. The file
    /// it names is the index of an entry.
    pub fn locate(&self, index: u64) -> Option<ChunkLocation> {
        self.layout.locate(index)
    }

    /// How the entries are cut into chunks. A directory or a link has none.
    pub fn layout(&self) -> &Layout {
        &self.layout
    }
}

/// Whether `name` is the kind of name given to a file that is still being
/// written.
pub fn is_part_name(name: &str) -> bool {
    path::is_part_name(name.as_bytes())
}

/// A path component that cannot escape the destination directory, be
/// reinterpreted on any supported platform, or be taken for a part file.
pub fn is_safe_component(component: &str) -> bool {
    path::is_safe_name(component.as_bytes())
}

/// The content id and the artifact id of a manifest, from its two encoded
/// halves. The artifact id is taken over the content id, not over the content
/// again, so the content is hashed once.
fn ids(content: &[u8], metadata: &[u8]) -> (Hash, Hash) {
    let content_id = Hash::derive(CONTENT_CONTEXT, content);
    let mut named = Vec::with_capacity(1 + content_id.as_bytes().len() + metadata.len());
    named.push(FORMAT_VERSION);
    named.extend_from_slice(content_id.as_bytes());
    named.extend_from_slice(metadata);
    (content_id, Hash::derive(ID_CONTEXT, &named))
}

/// A decoded value, if decoding it used every byte.
fn whole<T>((value, rest): (T, &[u8])) -> Result<T> {
    if rest.is_empty() {
        Ok(value)
    } else {
        Err(invalid("bytes are left over after the manifest"))
    }
}

fn invalid(reason: impl Into<String>) -> Error {
    Error::InvalidManifest(reason.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    const PLAIN: EntryMetadata = EntryMetadata {
        mode: 0o644,
        modified: None,
    };

    fn file(path: &str, size: u64) -> Entry {
        Entry {
            path: path.into(),
            kind: EntryKind::File { size },
        }
    }

    fn dir(path: &str) -> Entry {
        Entry {
            path: path.into(),
            kind: EntryKind::Directory,
        }
    }

    fn link(path: &str, target: &str) -> Entry {
        Entry {
            path: path.into(),
            kind: EntryKind::Symlink {
                target: target.into(),
            },
        }
    }

    /// A manifest of exactly these entries, with a digest for every chunk.
    fn manifest(entries: Vec<Entry>) -> Manifest {
        let chunk_size = MIN_CHUNK_SIZE;
        let chunks = entries
            .iter()
            .map(|entry| entry.size().div_ceil(u64::from(chunk_size)))
            .sum::<u64>();
        Manifest::new(
            "data",
            chunk_size,
            entries.into_iter().map(|entry| (entry, PLAIN)).collect(),
            (0..chunks).map(|i| Hash::of(&i.to_le_bytes())).collect(),
        )
    }

    /// A directory artifact: the root, then `entries`.
    fn tree(mut entries: Vec<Entry>) -> Manifest {
        entries.insert(0, dir(""));
        manifest(entries)
    }

    fn reason(manifest: &Manifest) -> String {
        manifest.validate().unwrap_err().to_string()
    }

    #[test]
    fn accepts_a_well_formed_tree() {
        tree(vec![
            file("a.bin", 10_000),
            dir("empty"),
            link("latest", "sub/b.bin"),
            dir("sub"),
            file("sub/b.bin", 1),
            link("sub/up", "../a.bin"),
        ])
        .validate()
        .unwrap();
        tree(vec![]).validate().unwrap();
        manifest(vec![file("", 5)]).validate().unwrap();
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
            let manifest = tree(vec![file(path, 1)]);
            assert!(reason(&manifest).contains("unsafe path"), "{path:?}");
        }
    }

    #[test]
    fn rejects_unsorted_or_duplicate_paths() {
        for entries in [
            vec![file("b", 1), file("a", 1)],
            vec![file("a", 1), file("a", 1)],
            vec![dir("a"), file("a", 1)],
        ] {
            assert!(reason(&tree(entries)).contains("sorted and unique"));
        }
    }

    /// A path's parent has to be a listed directory. That is what keeps
    /// anything from lying under a file, or behind a link that leads
    /// somewhere else.
    #[test]
    fn rejects_an_entry_that_is_not_inside_a_listed_directory() {
        for entries in [
            vec![file("sub/b", 1)],
            vec![file("a", 1), file("a/b", 1)],
            vec![link("a", "b"), file("a/c", 1), dir("b")],
            vec![dir("a"), file("a/b/c", 1)],
        ] {
            assert!(reason(&tree(entries)).contains("not inside a directory"));
        }
    }

    #[test]
    fn rejects_a_link_that_points_outside_the_artifact() {
        for (path, target) in [
            ("a", ".."),
            ("a", "../x"),
            ("a", "/etc/passwd"),
            ("a", "b/../../c"),
            ("a", ""),
            ("a", "b//c"),
        ] {
            let manifest = tree(vec![link(path, target)]);
            assert!(reason(&manifest).contains("points outside"), "{target:?}");
        }
        tree(vec![dir("d"), link("d/a", "..")]).validate().unwrap();
    }

    #[test]
    fn the_root_comes_first_and_is_a_file_or_a_directory() {
        assert!(reason(&manifest(vec![])).contains("at least its root"));
        assert!(reason(&manifest(vec![file("a", 1)])).contains("the first entry is the root"));
        assert!(reason(&manifest(vec![link("", "a")])).contains("cannot be a link"));
        let two = manifest(vec![file("", 1), file("a", 1)]);
        assert!(reason(&two).contains("exactly one file"));
    }

    #[test]
    fn rejects_wrong_chunk_count_and_chunk_size() {
        let mut manifest = tree(vec![file("a", 10_000)]);
        manifest.content.chunks.pop();
        assert!(reason(&manifest).contains("digests are listed"));

        let mut manifest = tree(vec![file("a", 1)]);
        manifest.content.chunk_size = 1;
        assert!(reason(&manifest).contains("chunk size"));
    }

    #[test]
    fn rejects_unsafe_artifact_name() {
        for name in ["..", "a/b", "", "a.mux-part"] {
            let mut manifest = tree(vec![file("a", 1)]);
            manifest.metadata.name = name.into();
            assert!(
                reason(&manifest).contains("unsafe artifact name"),
                "{name:?}"
            );
        }
    }

    #[test]
    fn rejects_metadata_that_does_not_fit_the_entries() {
        let mut manifest = tree(vec![file("a", 1)]);
        manifest.metadata.entries.pop();
        assert!(reason(&manifest).contains("different number of entries"));

        let mut manifest = tree(vec![file("a", 1)]);
        manifest.metadata.entries[1].mode = 0o4755;
        assert!(reason(&manifest).contains("beyond the permissions"));
        manifest.metadata.entries[1].mode = PERMISSION_BITS;
        manifest.validate().unwrap();

        let mut manifest = tree(vec![file("a", 1)]);
        let time = |nanos| Some(Timestamp { seconds: -5, nanos });
        manifest.metadata.entries[1].modified = time(1_000_000_000);
        assert!(reason(&manifest).contains("nanoseconds"));
        manifest.metadata.entries[1].modified = time(999_999_999);
        manifest.validate().unwrap();
    }

    /// These decide which manifests every node accepts, so a change to any
    /// of them is a change to the format.
    #[test]
    fn the_limits_are_what_the_format_says() {
        assert_eq!(FORMAT_VERSION, 1);
        assert_eq!(MIN_CHUNK_SIZE, 4096);
        assert_eq!(DEFAULT_CHUNK_SIZE, 1_048_576);
        assert_eq!(MAX_CHUNK_SIZE, 16_777_216);
        assert_eq!(MAX_MANIFEST_BYTES, 67_108_864);
        assert_eq!(MAX_NAME_BYTES, 246);
        assert_eq!(MAX_TARGET_BYTES, 4096);
        assert_eq!(PART_SUFFIX, ".mux-part");
        assert_eq!(PERMISSION_BITS, 0o777);
    }

    #[test]
    fn the_part_suffix_is_the_one_the_path_rules_check() {
        assert!(is_part_name(&format!("a{PART_SUFFIX}")));
        assert!(!is_safe_component(&format!("a{PART_SUFFIX}")));
        assert!(!is_part_name("a.part"));
        let named = tree(vec![file(&format!("a{PART_SUFFIX}"), 1)]);
        assert!(reason(&named).contains("unsafe path"));
    }

    #[test]
    fn chunk_hashes_come_from_the_manifest() {
        let manifest = tree(vec![file("a", 10_000)]);
        let chunks = manifest.content.chunks.clone();
        let artifact = Artifact::new(manifest).unwrap();
        assert_eq!(artifact.chunk_hash(0), Some(chunks[0]));
        assert_eq!(artifact.chunk_hash(2), Some(chunks[2]));
        assert_eq!(artifact.chunk_hash(3), None);
    }

    #[test]
    fn an_artifact_answers_for_its_manifest() {
        let artifact = Artifact::new(tree(vec![file("a", 1), dir("d")])).unwrap();
        assert_eq!(artifact.name(), "data");
        assert_eq!(artifact.entries().len(), 3);
        assert!(!artifact.is_file());
        assert_eq!(artifact.metadata(2), Some(PLAIN));
        assert_eq!(artifact.metadata(3), None);
        assert!(
            Artifact::new(manifest(vec![file("", 1)]))
                .unwrap()
                .is_file()
        );
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
        assert!(too_large(alloc::vec![0; MAX_MANIFEST_BYTES + 1]));
        assert!(!too_large(alloc::vec![0; MAX_MANIFEST_BYTES]));
    }

    #[test]
    fn locates_chunks_across_entries_including_those_with_none() {
        let cs = u64::from(MIN_CHUNK_SIZE);
        // Entry 0 is the root.
        let artifact = Artifact::new(tree(vec![
            file("a", cs * 2 + 5),
            file("b", 0),
            file("c", cs),
            dir("d"),
            link("e", "a"),
        ]))
        .unwrap();

        assert_eq!(artifact.chunk_count(), 4);
        assert_eq!(artifact.total_size(), cs * 3 + 5);
        let at = |i| artifact.locate(i).unwrap();
        assert_eq!((at(0).file, at(0).offset, u64::from(at(0).len)), (1, 0, cs));
        assert_eq!((at(2).file, at(2).offset, at(2).len), (1, cs * 2, 5));
        assert_eq!((at(3).file, at(3).offset, u64::from(at(3).len)), (3, 0, cs));
        assert!(artifact.locate(4).is_none());
        assert_eq!(artifact.file_chunks(0), 0..0);
        assert_eq!(artifact.file_chunks(2), 3..3);
        assert_eq!(artifact.file_chunks(3), 3..4);
        assert_eq!(artifact.file_chunks(4), 4..4);
        assert_eq!(artifact.file_chunks(5), 4..4);
    }

    #[test]
    fn decode_requires_matching_id() {
        let artifact = Artifact::new(tree(vec![file("a", 1)])).unwrap();
        let bytes = artifact.encoded().to_vec();

        let decoded = Artifact::decode(artifact.id(), bytes.clone()).unwrap();
        assert_eq!(decoded.manifest(), artifact.manifest());
        assert_eq!(decoded.content_id(), artifact.content_id());

        assert!(Artifact::decode(Hash::of(b"other"), bytes.clone()).is_err());
        let mut tampered = bytes;
        *tampered.last_mut().unwrap() ^= 1;
        assert!(Artifact::decode(artifact.id(), tampered).is_err());
    }

    /// A manifest in a format this build does not know is refused by name,
    /// not misread.
    #[test]
    fn an_unknown_format_version_is_refused() {
        let artifact = Artifact::new(tree(vec![file("a", 1)])).unwrap();
        let mut bytes = artifact.encoded().to_vec();
        assert_eq!(bytes[0], FORMAT_VERSION);

        bytes[0] = 2;
        let error = Artifact::decode(artifact.id(), bytes)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("format version 2 is not supported"),
            "{error}"
        );

        let empty = Artifact::decode(artifact.id(), Vec::new());
        assert!(empty.unwrap_err().to_string().contains("empty"));
    }

    /// The encoding is the version, the length of the content, the content
    /// and the metadata. Bytes cut short, or with something added, are
    /// refused.
    #[test]
    fn the_encoding_is_framed_and_nothing_else_decodes() {
        let artifact = Artifact::new(tree(vec![file("a", 1)])).unwrap();
        let bytes = artifact.encoded().to_vec();
        let content = postcard::to_allocvec(&artifact.manifest().content).unwrap();
        let metadata = postcard::to_allocvec(&artifact.manifest().metadata).unwrap();
        assert_eq!(bytes[0], FORMAT_VERSION);
        assert_eq!(bytes[1..9], (content.len() as u64).to_le_bytes());
        assert_eq!(bytes[9..9 + content.len()], content);
        assert_eq!(bytes[9 + content.len()..], metadata);
        assert_eq!(
            ids(&content, &metadata),
            (artifact.content_id(), artifact.id())
        );

        let refusal = |bytes: Vec<u8>, expected: Hash| {
            Artifact::decode(expected, bytes).unwrap_err().to_string()
        };
        for cut in [1, 5, 8, 9 + content.len() - 1] {
            let reason = refusal(bytes[..cut].to_vec(), artifact.id());
            assert!(reason.contains("cut short"), "{cut}: {reason}");
        }
        let mut too_long = bytes.clone();
        too_long[1..9].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(refusal(too_long, artifact.id()).contains("cut short"));

        // Extra bytes change the id. Even under their own id they are not a
        // manifest.
        let mut padded = bytes.clone();
        padded.push(0);
        assert!(refusal(padded.clone(), artifact.id()).contains("does not match"));
        let own = ids(&content, &padded[9 + content.len()..]).1;
        assert!(refusal(padded, own).contains("left over"));
    }

    /// Names, permissions and times are not content. Two artifacts that
    /// differ only in them are the same swarm, and a ticket still tells them
    /// apart.
    #[test]
    fn the_content_id_ignores_names_permissions_and_times() {
        let original = tree(vec![file("a", 10_000), dir("d"), link("l", "a")]);
        let ids = |manifest: &Manifest| {
            let artifact = Artifact::new(manifest.clone()).unwrap();
            (artifact.content_id(), artifact.id())
        };
        let (content, id) = ids(&original);
        assert_ne!(content, id);

        let mut renamed = original.clone();
        renamed.metadata.name = "other".into();
        let mut touched = original.clone();
        touched.metadata.entries[1].modified = Some(Timestamp {
            seconds: 1_700_000_000,
            nanos: 5,
        });
        let mut permitted = original.clone();
        permitted.metadata.entries[1].mode = 0o755;
        for changed in [renamed, touched, permitted] {
            assert_eq!(ids(&changed).0, content);
            assert_ne!(ids(&changed).1, id);
        }

        let mut resized = original.clone();
        resized.content.entries[1] = file("a", 10_001);
        let mut moved = original.clone();
        moved.content.entries[2] = dir("e");
        let mut retargeted = original.clone();
        retargeted.content.entries[3] = link("l", "d");
        let mut rehashed = original.clone();
        rehashed.content.chunks[0] = Hash::of(b"different");
        for changed in [resized, moved, retargeted, rehashed] {
            assert_ne!(ids(&changed).0, content);
        }
    }
}
