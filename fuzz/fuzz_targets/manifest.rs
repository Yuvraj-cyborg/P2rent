//! A manifest arrives from another machine before anything is known about
//! it. Whatever the bytes are, decoding and validating must not panic, and a
//! manifest that is accepted must be safe to act on.

#![no_main]

use libfuzzer_sys::fuzz_target;
use model::artifact::{Artifact, Manifest, is_safe_component};

fuzz_target!(|data: &[u8]| {
    let Ok(manifest) = postcard::from_bytes::<Manifest>(data) else {
        return;
    };
    if manifest.validate().is_err() {
        return;
    }

    // Accepted: no path may leave the destination directory.
    assert!(is_safe_component(&manifest.name));
    for file in &manifest.files {
        assert!(
            file.path.split('/').all(is_safe_component),
            "{:?}",
            file.path
        );
    }

    let files = manifest.files.clone();
    let chunk_size = manifest.chunk_size;
    let Ok(artifact) = Artifact::new(manifest) else {
        return;
    };

    // Every chunk lies inside its file, and the chunks of each file follow
    // one another without a gap.
    let mut covered = 0u64;
    for index in 0..artifact.chunk_count() {
        let chunk = artifact.locate(index).expect("index is in range");
        let size = files[chunk.file].size;
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
});
