//! A manifest arrives from another machine before anything is known about
//! it. Whatever the bytes are, decoding and validating must not panic, and a
//! manifest that is accepted must be safe to act on.

#![no_main]

use libfuzzer_sys::fuzz_target;
use model::artifact::path::{depth, is_safe_path, is_safe_target, parent_len};
use model::artifact::{Artifact, EntryKind, Manifest, is_safe_component};
use std::collections::BTreeSet;

fuzz_target!(|data: &[u8]| {
    let Ok(manifest) = postcard::from_bytes::<Manifest>(data) else {
        return;
    };
    if manifest.validate().is_err() {
        return;
    }

    // Accepted: nothing may leave the destination directory. Every path is
    // made of plain names, lies in a directory of the artifact (so not under
    // a file or behind a link), and no two are the same. A link points
    // inside the artifact.
    assert!(is_safe_component(&manifest.metadata.name));
    let entries = manifest.content.entries.clone();
    assert!(entries[0].path.is_empty());
    assert!(!matches!(entries[0].kind, EntryKind::Symlink { .. }));
    let mut directories = BTreeSet::new();
    let mut seen = BTreeSet::new();
    for entry in &entries {
        let path = entry.path.as_bytes();
        assert!(seen.insert(path), "{:?} is listed twice", entry.path);
        if !path.is_empty() {
            assert!(is_safe_path(path), "{:?}", entry.path);
            assert!(
                directories.contains(&path[..parent_len(path)]),
                "{:?} is not in a directory",
                entry.path
            );
        }
        match &entry.kind {
            EntryKind::File { .. } => {}
            EntryKind::Directory => {
                directories.insert(path);
            }
            EntryKind::Symlink { target } => {
                assert!(is_safe_target(target.as_bytes(), depth(path)));
            }
        }
    }
    assert_eq!(manifest.metadata.entries.len(), entries.len());

    let chunk_size = manifest.content.chunk_size;
    let Ok(artifact) = Artifact::new(manifest) else {
        return;
    };

    // Every chunk lies inside its file, and the chunks of each file follow
    // one another without a gap.
    let mut covered = 0u64;
    for index in 0..artifact.chunk_count() {
        let chunk = artifact.locate(index).expect("index is in range");
        let size = entries[chunk.file].size();
        assert!(chunk.len > 0 && chunk.len <= chunk_size);
        assert!(chunk.offset + u64::from(chunk.len) <= size);
        assert!(artifact.file_chunks(chunk.file).contains(&index));
        covered += u64::from(chunk.len);
    }
    assert_eq!(covered, artifact.total_size());
    assert!(artifact.locate(artifact.chunk_count()).is_none());

    // What was accepted decodes again to the same artifact.
    let again = Artifact::decode(artifact.id(), artifact.encoded().to_vec())
        .expect("an artifact's own encoding decodes");
    assert_eq!(again.manifest(), artifact.manifest());
    assert_eq!(again.content_id(), artifact.content_id());
});
