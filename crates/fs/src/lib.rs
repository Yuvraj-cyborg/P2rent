//! Local storage: hashing files into artifacts, and reading and writing
//! their chunks on disk.

// Every public item is documented.
#![warn(missing_docs)]
// A library must not panic on bad input. Tests may.
#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]

mod disk;
mod error;
mod index;
mod io;

pub use disk::DiskStore;
pub use error::{Error, Result};
pub use index::{Indexed, index, measure};

#[cfg(test)]
mod tests {
    use super::*;
    use model::artifact::{Kind, MIN_CHUNK_SIZE, PART_SUFFIX};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    const CS: usize = MIN_CHUNK_SIZE as usize;

    fn bytes(len: usize, seed: u8) -> Vec<u8> {
        (0..len)
            .map(|i| i.to_le_bytes()[0].wrapping_mul(31).wrapping_add(seed))
            .collect()
    }

    fn sample_tree(dir: &std::path::Path) -> std::path::PathBuf {
        let root = dir.join("bundle");
        fs::create_dir_all(root.join("nested/deep")).unwrap();
        fs::write(root.join("a.bin"), bytes(CS * 3 + 17, 1)).unwrap();
        fs::write(root.join("empty"), b"").unwrap();
        fs::write(root.join("nested/b.bin"), bytes(CS, 2)).unwrap();
        fs::write(root.join("nested/deep/c.bin"), bytes(5, 3)).unwrap();
        root
    }

    #[test]
    fn indexes_a_directory_deterministically() {
        let tmp = tempfile::tempdir().unwrap();
        let root = sample_tree(tmp.path());
        let hashed = AtomicU64::new(0);
        let count = |n| {
            hashed.fetch_add(n, Ordering::Relaxed);
        };

        let first = index(&root, MIN_CHUNK_SIZE, &count).unwrap();
        let second = index(&root, MIN_CHUNK_SIZE, &|_| {}).unwrap();

        let manifest = first.artifact.manifest();
        assert_eq!(manifest.kind, Kind::Directory);
        assert_eq!(manifest.name, "bundle");
        let paths: Vec<&str> = manifest.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            ["a.bin", "empty", "nested/b.bin", "nested/deep/c.bin"]
        );
        // a.bin: 4 chunks, empty: none, b.bin: 1, c.bin: 1.
        assert_eq!(first.artifact.chunk_count(), 6);
        assert_eq!(first.artifact.id(), second.artifact.id());
        assert_eq!(hashed.load(Ordering::Relaxed), first.artifact.total_size());
        assert_eq!(measure(&root).unwrap(), first.artifact.total_size());
    }

    #[test]
    fn content_changes_change_the_id() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("model.bin");
        fs::write(&file, bytes(CS * 2, 1)).unwrap();
        let before = index(&file, MIN_CHUNK_SIZE, &|_| {}).unwrap();
        assert_eq!(before.artifact.manifest().kind, Kind::File);

        fs::write(&file, bytes(CS * 2, 9)).unwrap();
        let after = index(&file, MIN_CHUNK_SIZE, &|_| {}).unwrap();
        assert_ne!(before.artifact.id(), after.artifact.id());
    }

    /// A destination next to the sample tree, the artifact, and a store over
    /// the original files to copy chunks from.
    fn sample(tmp: &std::path::Path) -> (Arc<model::artifact::Artifact>, DiskStore, PathBuf) {
        let root = sample_tree(tmp);
        let indexed = index(&root, MIN_CHUNK_SIZE, &|_| {}).unwrap();
        let artifact = Arc::new(indexed.artifact);
        let source = DiskStore::whole(artifact.clone(), indexed.root);
        (artifact, source, tmp.join("out"))
    }

    fn resume(artifact: &Arc<model::artifact::Artifact>, out: &std::path::Path) -> DiskStore {
        DiskStore::resume(artifact.clone(), out, &|_| {}).unwrap()
    }

    fn copy(source: &DiskStore, dest: &DiskStore, chunk: u64) {
        assert!(
            dest.write_chunk(chunk, &source.read_chunk(chunk).unwrap())
                .unwrap()
        );
    }

    fn part(path: &std::path::Path) -> PathBuf {
        let mut name = path.as_os_str().to_owned();
        name.push(PART_SUFFIX);
        PathBuf::from(name)
    }

    #[test]
    fn copies_chunk_by_chunk_and_resumes() {
        let tmp = tempfile::tempdir().unwrap();
        let (artifact, source, out) = sample(tmp.path());
        let dest = resume(&artifact, &out);
        assert_eq!(dest.held().count_ones(), 0);
        assert!(!dest.is_complete());

        // Write every chunk except the last, in reverse order.
        let total = artifact.chunk_count();
        for chunk in (0..total - 1).rev() {
            copy(&source, &dest, chunk);
        }

        // Opening it again finds what was written.
        let dest = resume(&artifact, &out);
        assert_eq!(dest.held().count_ones(), total - 1);
        assert!(!dest.has(total - 1));

        copy(&source, &dest, total - 1);
        assert!(dest.is_complete());
        dest.flush().unwrap();
        dest.flush().unwrap();
        assert!(resume(&artifact, &out).is_complete());

        for entry in &artifact.manifest().files {
            assert_eq!(
                fs::read(source.root().join(&entry.path)).unwrap(),
                fs::read(out.join("bundle").join(&entry.path)).unwrap(),
                "{}",
                entry.path
            );
        }
    }

    #[test]
    fn a_file_has_its_final_name_only_once_it_is_whole() {
        let tmp = tempfile::tempdir().unwrap();
        let (artifact, source, out) = sample(tmp.path());
        let dest = resume(&artifact, &out);
        let a = out.join("bundle/a.bin");

        // A file with no chunks has nothing to wait for.
        assert_eq!(fs::read(out.join("bundle/empty")).unwrap(), b"");
        assert!(!part(&out.join("bundle/empty")).exists());

        // a.bin is chunks 0 to 3.
        for chunk in [3, 0, 2] {
            copy(&source, &dest, chunk);
            assert!(!a.exists(), "a.bin appeared before chunk 1 arrived");
            assert!(part(&a).exists());
        }
        copy(&source, &dest, 1);
        assert_eq!(
            fs::read(&a).unwrap(),
            fs::read(source.root().join("a.bin")).unwrap()
        );
        assert!(!part(&a).exists());
        assert!(!out.join("bundle/nested/b.bin").exists());
    }

    #[test]
    fn serves_what_it_holds_from_part_files_and_whole_files() {
        let tmp = tempfile::tempdir().unwrap();
        let (artifact, source, out) = sample(tmp.path());
        let dest = resume(&artifact, &out);

        copy(&source, &dest, 2);
        assert_eq!(
            dest.read_chunk(2).unwrap(),
            source.read_chunk(2).unwrap(),
            "from a part file"
        );
        assert!(dest.read_chunk(1).is_err(), "chunk 1 is not held");

        for chunk in [0, 1, 3] {
            copy(&source, &dest, chunk);
        }
        for chunk in 0..4 {
            assert_eq!(
                dest.read_chunk(chunk).unwrap(),
                source.read_chunk(chunk).unwrap(),
                "from the whole file"
            );
        }
        assert!(dest.read_chunk(artifact.chunk_count()).is_err());
    }

    #[test]
    fn a_file_already_there_is_replaced_whole_or_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let (artifact, source, out) = sample(tmp.path());
        let a = out.join("bundle/a.bin");
        let b = out.join("bundle/nested/b.bin");
        fs::create_dir_all(out.join("bundle/nested")).unwrap();

        // The right length with one wrong chunk, and the wrong length.
        let mut nearly = fs::read(source.root().join("a.bin")).unwrap();
        nearly[CS + 1] ^= 0xff;
        fs::write(&a, &nearly).unwrap();
        fs::write(&b, b"something else").unwrap();

        // The three chunks of a.bin that are right are reused, not fetched.
        let dest = resume(&artifact, &out);
        assert!(dest.has(0) && dest.has(2) && dest.has(3));
        assert!(!dest.has(1) && !dest.has(4));
        assert_eq!(dest.read_chunk(2).unwrap(), source.read_chunk(2).unwrap());
        assert_eq!(fs::read(&a).unwrap(), nearly, "changed before it was whole");
        assert_eq!(fs::read(&b).unwrap(), b"something else");

        copy(&source, &dest, 1);
        copy(&source, &dest, 4);
        assert_eq!(
            fs::read(&a).unwrap(),
            fs::read(source.root().join("a.bin")).unwrap()
        );
        assert_eq!(
            fs::read(&b).unwrap(),
            fs::read(source.root().join("nested/b.bin")).unwrap()
        );
    }

    #[test]
    fn a_whole_file_already_there_is_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let (artifact, source, out) = sample(tmp.path());
        let a = out.join("bundle/a.bin");
        fs::create_dir_all(out.join("bundle")).unwrap();
        fs::copy(source.root().join("a.bin"), &a).unwrap();
        // Left over from a download of something else.
        fs::write(part(&a), b"stale").unwrap();

        let dest = resume(&artifact, &out);
        assert_eq!(dest.held().count_ones(), 4);
        assert!(!part(&a).exists(), "the stale part file was kept");
        assert_eq!(dest.read_chunk(3).unwrap(), source.read_chunk(3).unwrap());
    }

    #[test]
    fn resuming_keeps_the_right_chunks_of_a_part_file() {
        let tmp = tempfile::tempdir().unwrap();
        let (artifact, source, out) = sample(tmp.path());
        let a = out.join("bundle/a.bin");
        let dest = resume(&artifact, &out);
        for chunk in [0, 1, 2] {
            copy(&source, &dest, chunk);
        }
        drop(dest);

        // Damage chunk 1 and cut the file short inside chunk 2.
        let mut data = fs::read(part(&a)).unwrap();
        data[CS + 1] ^= 0xff;
        data.truncate(CS * 2 + 1);
        fs::write(part(&a), data).unwrap();

        let dest = resume(&artifact, &out);
        assert!(dest.has(0));
        assert!(!dest.has(1) && !dest.has(2) && !dest.has(3));
        for chunk in [1, 2, 3] {
            copy(&source, &dest, chunk);
        }
        assert_eq!(
            fs::read(&a).unwrap(),
            fs::read(source.root().join("a.bin")).unwrap()
        );
    }

    #[test]
    fn a_part_file_that_is_already_whole_is_renamed_on_resume() {
        let tmp = tempfile::tempdir().unwrap();
        let (artifact, source, out) = sample(tmp.path());
        let b = out.join("bundle/nested/b.bin");
        // As if the program stopped between the last write and the rename.
        fs::create_dir_all(out.join("bundle/nested")).unwrap();
        fs::copy(source.root().join("nested/b.bin"), part(&b)).unwrap();

        let dest = resume(&artifact, &out);
        assert!(dest.has(4));
        assert!(b.exists() && !part(&b).exists());
    }

    #[test]
    fn rejects_bytes_that_are_not_the_chunk() {
        let tmp = tempfile::tempdir().unwrap();
        let (artifact, source, out) = sample(tmp.path());
        let dest = resume(&artifact, &out);
        let right = source.read_chunk(0).unwrap();

        let mut wrong = right.clone();
        wrong[7] ^= 1;
        assert!(!dest.write_chunk(0, &wrong).unwrap());
        assert!(!dest.write_chunk(0, &right[1..]).unwrap(), "too short");
        // Chunk 4 is the same length, from another file.
        let other = source.read_chunk(4).unwrap();
        assert!(
            !dest.write_chunk(0, &other).unwrap(),
            "another chunk's bytes"
        );
        assert!(!dest.has(0) && !dest.has(4));
        assert!(dest.write_chunk(artifact.chunk_count(), &right).is_err());

        assert!(dest.write_chunk(0, &right).unwrap());
        assert!(
            dest.write_chunk(0, &right).unwrap(),
            "a second time is fine"
        );
        assert!(dest.has(0));
    }

    #[test]
    fn something_that_is_not_a_file_in_the_way_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let (artifact, _source, out) = sample(tmp.path());
        fs::create_dir_all(out.join("bundle/a.bin")).unwrap();
        assert!(DiskStore::resume(artifact, &out, &|_| {}).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn an_executable_file_arrives_executable() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let root = sample_tree(tmp.path());
        let script = root.join("run.sh");
        fs::write(&script, b"#!/bin/sh\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

        let indexed = index(&root, MIN_CHUNK_SIZE, &|_| {}).unwrap();
        let artifact = Arc::new(indexed.artifact);
        let source = DiskStore::whole(artifact.clone(), indexed.root);
        let out = tmp.path().join("out");
        let dest = resume(&artifact, &out);
        for chunk in 0..artifact.chunk_count() {
            copy(&source, &dest, chunk);
        }

        let mode = |path: &str| {
            fs::metadata(out.join("bundle").join(path))
                .unwrap()
                .permissions()
                .mode()
        };
        assert_ne!(mode("run.sh") & 0o111, 0);
        assert_eq!(mode("a.bin") & 0o111, 0);
    }

    #[test]
    fn indexing_leaves_out_part_files() {
        let tmp = tempfile::tempdir().unwrap();
        let root = sample_tree(tmp.path());
        let before = index(&root, MIN_CHUNK_SIZE, &|_| {}).unwrap();

        fs::write(part(&root.join("a.bin")), b"unfinished").unwrap();
        fs::create_dir(part(&root.join("nested/folder"))).unwrap();
        fs::write(part(&root.join("nested/folder")).join("inside"), b"x").unwrap();

        let after = index(&root, MIN_CHUNK_SIZE, &|_| {}).unwrap();
        assert_eq!(after.artifact.id(), before.artifact.id());
        assert_eq!(after.skipped, before.skipped + 2);
    }
}
