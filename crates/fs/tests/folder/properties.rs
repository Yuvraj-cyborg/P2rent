//! Properties of a store over a local folder, checked on generated inputs.

use crate::common::Store;
use fs::index;
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

    /// Reordering, inserting, deleting or repeating bytes in the old file
    /// can only change how much is fetched. Even hints for the wrong
    /// bytes cannot change what gets committed.
    #[test]
    fn edited_files_are_reconstructed(
        seed in any::<u32>(), old_len in 0..12usize * MIN_CHUNK_SIZE as usize,
        edits in vec((any::<u16>(), any::<u16>(), 0usize..8), 1..9),
        wrong_hints in any::<bool>(),
    ) {
        let mut state = seed;
        let old: Vec<u8> = (0..old_len).map(|_| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            state.to_be_bytes()[0]
        }).collect();
        let mut new = Vec::new();
        for (a, b, padding) in edits {
            let offset = |n| usize::try_from(u64::from(n) * old.len() as u64 / u64::from(u16::MAX)).unwrap();
            new.extend_from_slice(&old[offset(a.min(b))..offset(a.max(b))]);
            new.extend(std::iter::repeat_n(7, padding));
        }
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("sample");
        std::fs::write(&path, &new).unwrap();
        let out = tmp.path().join("out");
        std::fs::create_dir(&out).unwrap();
        let target = out.join("sample");
        std::fs::write(&target, &old).unwrap();
        let indexed = index(&path, MIN_CHUNK_SIZE, &|_| {}).unwrap();
        let artifact = Arc::new(indexed.artifact);
        let place = Arc::new(fs::Folder::inside(artifact.tree().clone(), &out));
        let hints = if wrong_hints {
            model::search::ChunkHints::pack((0..artifact.chunk_count()).map(|_| 0))
        } else {
            model::search::ChunkHints::pack(new.chunks(MIN_CHUNK_SIZE as usize)
                .map(|bytes| model::rolling::Rolling::of(bytes).value()))
        };
        let store = crate::common::wait(engine::Store::resume_with_hints(
            artifact.clone(), place, |_| {}, std::future::ready(Some(hints)),
        )).unwrap();
        if !store.is_complete() {
            prop_assert_eq!(std::fs::read(&target).unwrap(), old);
        }
        for (index, chunk) in (0..).zip(new.chunks(MIN_CHUNK_SIZE as usize)) {
            if !store.has(index) {
                crate::common::wait(store.write_chunk(index, chunk.to_vec())).unwrap();
            }
        }
        crate::common::wait(store.finish()).unwrap();
        prop_assert!(store.is_complete());
        prop_assert_eq!(std::fs::read(&target).unwrap(), new);
    }

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
        let source = Store::whole(artifact.clone(), indexed.root);
        let out = tmp.path().join("out");
        let destination = Store::resume(artifact.clone(), &out).unwrap();
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
                // Entry 0 is the root, and the files follow it in order.
                let whole = artifact.file_chunks(number + 1).all(|index| destination.has(index));
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
        let reopened = Store::resume(artifact.clone(), &out).unwrap();
        prop_assert!(reopened.is_complete());
        for (number, contents) in files.iter().enumerate() {
            prop_assert_eq!(&written(number).unwrap(), contents);
        }
    }
}
