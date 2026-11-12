//! Local storage: hashing files into artifacts and reading or writing chunks
//! in place.

mod disk;
mod index;
mod io;

pub use disk::DiskStore;
pub use index::{Indexed, index, measure};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::{Kind, MIN_CHUNK_SIZE};
    use std::fs;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    const CS: usize = MIN_CHUNK_SIZE as usize;

    fn bytes(len: usize, seed: u8) -> Vec<u8> {
        (0..len)
            .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
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

    #[test]
    fn copies_chunk_by_chunk_and_resumes() {
        let tmp = tempfile::tempdir().unwrap();
        let root = sample_tree(tmp.path());
        let indexed = index(&root, MIN_CHUNK_SIZE, &|_| {}).unwrap();
        let artifact = Arc::new(indexed.artifact);
        let source = DiskStore::new(artifact.clone(), indexed.root);

        let out = tmp.path().join("out");
        let dest = DiskStore::inside(artifact.clone(), &out);
        assert_eq!(dest.scan(&|_| {}).unwrap().count_ones(), 0);
        dest.allocate().unwrap();

        // Write every chunk except the last, in reverse order.
        let total = artifact.chunk_count();
        for chunk in (0..total - 1).rev() {
            dest.write_chunk(chunk, &source.read_chunk(chunk).unwrap())
                .unwrap();
        }
        let have = dest.scan(&|_| {}).unwrap();
        assert_eq!(have.count_ones(), total - 1);
        assert!(!have.get(total - 1));

        // A second allocate must not disturb what is already there.
        dest.allocate().unwrap();
        dest.write_chunk(total - 1, &source.read_chunk(total - 1).unwrap())
            .unwrap();
        dest.finalize().unwrap();
        assert!(dest.scan(&|_| {}).unwrap().is_full());

        for entry in &artifact.manifest().files {
            assert_eq!(
                fs::read(root.join(&entry.path)).unwrap(),
                fs::read(out.join("bundle").join(&entry.path)).unwrap(),
                "{}",
                entry.path
            );
        }
    }

    #[test]
    fn scan_detects_corruption_and_truncation() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("model.bin");
        fs::write(&file, bytes(CS * 4, 1)).unwrap();
        let indexed = index(&file, MIN_CHUNK_SIZE, &|_| {}).unwrap();
        let store = DiskStore::new(Arc::new(indexed.artifact), indexed.root);

        let mut data = fs::read(&file).unwrap();
        data[CS + 1] ^= 0xff;
        data.truncate(CS * 3 + 1);
        fs::write(&file, data).unwrap();

        let have = store.scan(&|_| {}).unwrap();
        assert!(have.get(0) && have.get(2));
        assert!(!have.get(1) && !have.get(3));
    }

    #[test]
    fn rejects_chunks_of_the_wrong_length() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("model.bin");
        fs::write(&file, bytes(CS + 10, 1)).unwrap();
        let indexed = index(&file, MIN_CHUNK_SIZE, &|_| {}).unwrap();
        let store = DiskStore::new(Arc::new(indexed.artifact), indexed.root);
        assert!(store.write_chunk(1, &[0; 11]).is_err());
        assert!(store.write_chunk(2, &[0; 10]).is_err());
        assert!(store.read_chunk(2).is_err());
    }
}
