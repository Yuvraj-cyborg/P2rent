//! Properties of on-disk storage, checked on generated inputs.

use p2rent_core::artifact::MIN_CHUNK_SIZE;
use p2rent_fs::{DiskStore, index};
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

    /// Chunks may arrive in any order, and some may arrive twice. The
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
        let source = DiskStore::new(artifact.clone(), indexed.root);
        let out = tmp.path().join("out");
        let destination = DiskStore::inside(artifact.clone(), &out);
        destination.allocate().unwrap();

        // A shuffled order with repeats, then whatever it missed.
        let total = artifact.chunk_count();
        let mut sequence: Vec<u64> = match usize::try_from(total).unwrap() {
            0 => Vec::new(),
            count => order.iter().map(|pick| pick.index(count) as u64).collect(),
        };
        sequence.extend((0..total).rev());
        for chunk in sequence {
            destination
                .write_chunk(chunk, &source.read_chunk(chunk).unwrap())
                .unwrap();
        }
        destination.finalize().unwrap();

        let have = destination.scan(&|_| {}).unwrap();
        prop_assert!(have.is_full());
        for (number, contents) in files.iter().enumerate() {
            let written = std::fs::read(out.join("tree").join(format!("file{number}"))).unwrap();
            prop_assert_eq!(&written, contents);
        }
    }
}
