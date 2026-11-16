//! Properties of on-disk storage, checked on generated inputs.

use fs::{DiskStore, index};
use model::artifact::MIN_CHUNK_SIZE;
use proptest::collection::vec;
use proptest::prelude::*;
use std::sync::Arc;

/// File contents: a few files, some empty, some spanning several chunks.
fn files() -> impl Strategy<Value = Vec<Vec<u8>>> {
    let size = prop_oneof![
        Just(0usize),
        1usize..200,
        (1usize..4).prop_map(|n| n * MIN_CHUNK_SIZE as usize),
        4000usize..20_000,
    ];
    vec(
        (size, any::<u8>()).prop_map(|(len, seed)| {
            (0..len)
                .map(|i| i.to_le_bytes()[0].wrapping_mul(31).wrapping_add(seed))
                .collect()
        }),
        1..5,
    )
}

proptest! {
    // Each case writes real files, so fewer cases than the default.
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// Chunks may arrive in any order, and some may arrive twice. At every
    /// moment, a file under its final name is whole and right, and the
    /// destination ends up byte for byte the same as the source.
    #[test]
    fn chunks_can_arrive_in_any_order(
        files in files(),
        order in vec(any::<prop::sample::Index>(), 0..64),
    ) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("tree");
        std::fs::create_dir(&root).unwrap();
        for (number, contents) in files.iter().enumerate() {
            std::fs::write(root.join(format!("file{number}")), contents).unwrap();
        }

        let indexed = index(&root, MIN_CHUNK_SIZE, &|_| {}).unwrap();
        let artifact = Arc::new(indexed.artifact);
        let source = DiskStore::whole(artifact.clone(), indexed.root);
        let out = tmp.path().join("out");
        let destination = DiskStore::resume(artifact.clone(), &out, &|_| {}).unwrap();
        let written = |number: usize| std::fs::read(out.join("tree").join(format!("file{number}")));

        // A shuffled order with repeats, then whatever it missed.
        let total = artifact.chunk_count();
        let mut sequence: Vec<u64> = match usize::try_from(total).unwrap() {
            0 => Vec::new(),
            count => order.iter().map(|pick| pick.index(count) as u64).collect(),
        };
        sequence.extend((0..total).rev());
        for chunk in sequence {
            prop_assert!(destination.write_chunk(chunk, &source.read_chunk(chunk).unwrap()).unwrap());
            for (number, contents) in files.iter().enumerate() {
                let whole = artifact.file_chunks(number).all(|index| destination.has(index));
                match written(number) {
                    Ok(bytes) => {
                        prop_assert!(whole, "file{} has its name before it is whole", number);
                        prop_assert_eq!(&bytes, contents);
                    }
                    Err(_) => prop_assert!(!whole, "file{} is whole but has no name", number),
                }
            }
        }

        prop_assert!(destination.is_complete());
        let reopened = DiskStore::resume(artifact.clone(), &out, &|_| {}).unwrap();
        prop_assert!(reopened.is_complete());
        for (number, contents) in files.iter().enumerate() {
            prop_assert_eq!(&written(number).unwrap(), contents);
        }
    }
}
