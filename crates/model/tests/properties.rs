//! Properties of the data model, checked on generated inputs.

use model::artifact::{
    Artifact, Bitfield, Entry, EntryKind, EntryMetadata, Hash, MIN_CHUNK_SIZE, Manifest, Timestamp,
};
use model::hex;
use model::schedule::{ChunkState, Schedule};
use proptest::collection::{btree_set, vec};
use proptest::prelude::*;
use std::collections::BTreeSet;

/// A bitfield with the given bits set.
fn bitfield(len: u64, set: &BTreeSet<u64>) -> Bitfield {
    let mut bits = Bitfield::new(len);
    for &index in set {
        bits.set(index);
    }
    bits
}

/// A length and a set of indices below it.
fn length_and_indices() -> impl Strategy<Value = (u64, BTreeSet<u64>)> {
    (0u64..300).prop_flat_map(|len| (Just(len), btree_set(0..len.max(1), 0..64)))
}

/// What a generated entry is, before it has a path.
#[derive(Clone, Debug)]
enum Sort {
    File(u64),
    EmptyDirectory,
    /// A link to the entry's sibling of this number.
    Link(u8),
}

/// A valid directory manifest: the root, a few directories, and files, empty
/// directories and links inside them, in path order, with permissions and
/// times, and one digest per chunk.
fn manifest() -> impl Strategy<Value = Manifest> {
    let size = prop_oneof![
        Just(0u64),
        0u64..40_000,
        (0u64..10).prop_map(|n| n * u64::from(MIN_CHUNK_SIZE)),
    ];
    let sort = prop_oneof![
        4 => size.prop_map(Sort::File),
        1 => Just(Sort::EmptyDirectory),
        1 => (0u8..20).prop_map(Sort::Link),
    ];
    // Where an entry goes: at the top, or in one of four directories.
    let place = prop_oneof![Just(None), (0u8..4).prop_map(Some)];
    let metadata = (
        0u16..=0o777,
        proptest::option::of((any::<i64>(), 0u32..1_000_000_000)),
    )
        .prop_map(|(mode, modified)| EntryMetadata {
            mode,
            modified: modified.map(|(seconds, nanos)| Timestamp { seconds, nanos }),
        });
    vec((place, 0u8..20, sort, metadata), 0..12).prop_map(|generated| {
        let plain = EntryMetadata {
            mode: 0o755,
            modified: None,
        };
        let directory = |path: String| {
            let kind = EntryKind::Directory;
            (path.clone(), (Entry { path, kind }, plain))
        };
        // Keyed by path, which sorts them and drops repeats.
        let mut entries = std::collections::BTreeMap::new();
        entries.extend([directory(String::new())]);
        for (place, number, sort, metadata) in generated {
            let (prefix, target_prefix) = match place {
                None => (String::new(), String::new()),
                Some(d) => {
                    entries.extend([directory(format!("d{d}"))]);
                    (format!("d{d}/"), String::new())
                }
            };
            let path = format!("{prefix}e{number}");
            let kind = match sort {
                Sort::File(size) => EntryKind::File { size },
                Sort::EmptyDirectory => EntryKind::Directory,
                Sort::Link(sibling) => EntryKind::Symlink {
                    target: format!("{target_prefix}e{sibling}"),
                },
            };
            entries
                .entry(path.clone())
                .or_insert((Entry { path, kind }, metadata));
        }
        let entries: Vec<(Entry, EntryMetadata)> = entries.into_values().collect();
        let chunks: u64 = entries
            .iter()
            .map(|(entry, _)| entry.size().div_ceil(u64::from(MIN_CHUNK_SIZE)))
            .sum();
        Manifest::new(
            "tree",
            MIN_CHUNK_SIZE,
            entries,
            (0..chunks).map(|i| Hash::of(&i.to_le_bytes())).collect(),
        )
    })
}

/// Every chunk as (entry, offset, length), worked out the slow, obvious way.
fn naive_layout(manifest: &Manifest) -> Vec<(usize, u64, u32)> {
    let chunk_size = u64::from(manifest.content.chunk_size);
    let mut layout = Vec::new();
    for (index, entry) in manifest.content.entries.iter().enumerate() {
        let mut offset = 0;
        while offset < entry.size() {
            let len = (entry.size() - offset).min(chunk_size);
            layout.push((index, offset, u32::try_from(len).unwrap()));
            offset += len;
        }
    }
    layout
}

/// One step a download can take.
#[derive(Clone, Debug)]
enum Step {
    Claim {
        available: BTreeSet<u64>,
        cursor: u64,
    },
    Complete(u64),
    Release(u64),
}

fn steps(len: u64) -> impl Strategy<Value = Vec<Step>> {
    let index = 0..len + 2;
    vec(
        prop_oneof![
            (btree_set(0..len.max(1), 0..16), any::<u64>())
                .prop_map(|(available, cursor)| Step::Claim { available, cursor }),
            index.clone().prop_map(Step::Complete),
            index.prop_map(Step::Release),
        ],
        0..48,
    )
}

proptest! {
    #[test]
    fn a_bitfield_behaves_like_a_set((len, set) in length_and_indices()) {
        let set: BTreeSet<u64> = set.into_iter().filter(|&i| i < len).collect();
        let mut bits = bitfield(len, &set);

        prop_assert_eq!(bits.len(), len);
        prop_assert_eq!(bits.count_ones(), set.len() as u64);
        prop_assert_eq!(bits.is_full(), set.len() as u64 == len);
        prop_assert!(bits.is_well_formed(len));
        for index in 0..len + 8 {
            prop_assert_eq!(bits.get(index), set.contains(&index));
        }

        for &index in &set {
            bits.clear(index);
        }
        prop_assert_eq!(bits.count_ones(), 0);
        prop_assert!(bits.is_well_formed(len));
    }

    #[test]
    fn a_full_bitfield_has_every_bit(len in 0u64..2000) {
        let bits = Bitfield::full(len);
        prop_assert_eq!(bits.count_ones(), len);
        prop_assert!(bits.is_full());
        prop_assert!(bits.is_well_formed(len));
        prop_assert!(!bits.get(len));
    }

    #[test]
    fn out_of_range_indices_change_nothing((len, set) in length_and_indices(), beyond in 0u64..1000) {
        let set: BTreeSet<u64> = set.into_iter().filter(|&i| i < len).collect();
        let before = bitfield(len, &set);
        let mut after = before.clone();
        after.set(len + beyond);
        after.clear(len + beyond);
        prop_assert_eq!(before, after);
    }

    /// Whatever bytes arrive from another machine, a bitfield that passes
    /// the well-formedness check never claims a chunk past its end.
    #[test]
    fn a_well_formed_wire_bitfield_stays_in_range(
        len in 0u64..200,
        bytes in vec(any::<u8>(), 0..40),
        expected in 0u64..200,
    ) {
        // A bitfield is encoded as its length followed by its bytes.
        let wire = postcard_pair(len, &bytes);
        let Ok(bits) = decode_bitfield(&wire) else { return Ok(()) };
        if bits.is_well_formed(expected) {
            prop_assert_eq!(bits.len(), expected);
            prop_assert!(bits.count_ones() <= expected);
            prop_assert!(!bits.get(expected));
        }
    }

    #[test]
    fn a_manifest_survives_encoding(manifest in manifest()) {
        let artifact = Artifact::new(manifest.clone()).unwrap();
        let decoded = Artifact::decode(artifact.id(), artifact.encoded().to_vec()).unwrap();
        prop_assert_eq!(decoded.manifest(), &manifest);
        prop_assert_eq!(decoded.id(), artifact.id());
        prop_assert_eq!(decoded.content_id(), artifact.content_id());
    }

    /// Names, permissions and times are not content: change only them and
    /// the swarm is the same.
    #[test]
    fn the_content_id_depends_on_content_alone(
        manifest in manifest(),
        name in "[a-z]{1,8}",
        mode in 0u16..=0o777,
        seconds in any::<i64>(),
    ) {
        let original = Artifact::new(manifest.clone()).unwrap();
        let mut changed = manifest;
        changed.metadata.name = name;
        for metadata in &mut changed.metadata.entries {
            metadata.mode = mode;
            metadata.modified = Some(Timestamp { seconds, nanos: 0 });
        }
        let changed = Artifact::new(changed).unwrap();
        prop_assert_eq!(changed.content_id(), original.content_id());
    }

    #[test]
    fn a_tampered_manifest_is_rejected(manifest in manifest(), position in any::<usize>(), flip in 1u8..=255) {
        let artifact = Artifact::new(manifest).unwrap();
        let mut bytes = artifact.encoded().to_vec();
        let position = position % bytes.len();
        bytes[position] ^= flip;
        prop_assert!(Artifact::decode(artifact.id(), bytes).is_err());
    }

    #[test]
    fn locate_matches_the_obvious_layout(manifest in manifest()) {
        let expected = naive_layout(&manifest);
        let artifact = Artifact::new(manifest).unwrap();

        prop_assert_eq!(artifact.chunk_count(), expected.len() as u64);
        for (index, &(file, offset, len)) in expected.iter().enumerate() {
            let found = artifact.locate(index as u64).unwrap();
            prop_assert_eq!((found.file, found.offset, found.len), (file, offset, len));
        }
        prop_assert!(artifact.locate(expected.len() as u64).is_none());

        let covered: u64 = expected.iter().map(|&(_, _, len)| u64::from(len)).sum();
        prop_assert_eq!(covered, artifact.total_size());
    }

    #[test]
    fn file_chunks_split_the_index_range(manifest in manifest()) {
        let files = manifest.content.entries.len();
        let artifact = Artifact::new(manifest).unwrap();
        let mut next = 0;
        for file in 0..files {
            let range = artifact.file_chunks(file);
            prop_assert_eq!(range.start, next);
            for index in range.clone() {
                prop_assert_eq!(artifact.locate(index).unwrap().file, file);
            }
            next = range.end;
        }
        prop_assert_eq!(next, artifact.chunk_count());
    }

    #[test]
    fn hex_round_trips(bytes in vec(any::<u8>(), 0..80)) {
        let text = hex::encode(&bytes);
        prop_assert_eq!(text.len(), bytes.len() * 2);
        prop_assert_eq!(hex::decode(&text), Some(bytes));
    }

    #[test]
    fn hex_decoding_never_panics(text in ".{0,80}") {
        if let Some(bytes) = hex::decode(&text) {
            prop_assert_eq!(hex::encode(&bytes), text.to_ascii_lowercase());
        }
        let _ = hex::decode_32(&text);
        let _ = text.parse::<Hash>();
    }

    /// The schedule is compared, step by step, with a plain list of states.
    #[test]
    fn the_schedule_matches_a_simple_model(
        (len, have, steps) in (0u64..12).prop_flat_map(|len| {
            (Just(len), btree_set(0..len.max(1), 0..12), steps(len))
        }),
    ) {
        let have: BTreeSet<u64> = have.into_iter().filter(|&i| i < len).collect();
        let mut schedule = Schedule::new(&bitfield(len, &have));
        let mut model: Vec<ChunkState> = (0..len)
            .map(|i| if have.contains(&i) { ChunkState::Done } else { ChunkState::Missing })
            .collect();

        for step in steps {
            match step {
                Step::Claim { available, mut cursor } => {
                    let available: BTreeSet<u64> =
                        available.into_iter().filter(|&i| i < len).collect();
                    // The first candidate at or after the cursor, wrapping.
                    let expected = (0..len)
                        .map(|step| (cursor % len.max(1) + step) % len.max(1))
                        .find(|&i| {
                            model[usize::try_from(i).unwrap()] == ChunkState::Missing
                                && available.contains(&i)
                        });
                    let claimed = schedule.claim(&bitfield(len, &available), &mut cursor);
                    prop_assert_eq!(claimed, expected);
                    if let Some(index) = claimed {
                        model[usize::try_from(index).unwrap()] = ChunkState::InFlight;
                        prop_assert_eq!(cursor, (index + 1) % len);
                    }
                }
                Step::Complete(index) => {
                    let slot = usize::try_from(index).unwrap();
                    let in_flight = model.get(slot) == Some(&ChunkState::InFlight);
                    prop_assert_eq!(schedule.complete(index), in_flight);
                    if in_flight {
                        model[slot] = ChunkState::Done;
                    }
                }
                Step::Release(index) => {
                    let slot = usize::try_from(index).unwrap();
                    let in_flight = model.get(slot) == Some(&ChunkState::InFlight);
                    prop_assert_eq!(schedule.release(index), in_flight);
                    if in_flight {
                        model[slot] = ChunkState::Missing;
                    }
                }
            }

            let states: Vec<ChunkState> =
                (0..len).map(|i| schedule.state(i).unwrap()).collect();
            prop_assert_eq!(&states, &model);
            let not_done = model.iter().filter(|&&s| s != ChunkState::Done).count();
            prop_assert_eq!(schedule.remaining(), not_done as u64);
            prop_assert_eq!(schedule.is_complete(), not_done == 0);
        }
    }
}

/// The wire encoding of a `(u64, bytes)` pair, which is what a bitfield is.
fn postcard_pair(len: u64, bytes: &[u8]) -> Vec<u8> {
    postcard::to_allocvec(&(len, bytes)).unwrap()
}

fn decode_bitfield(wire: &[u8]) -> postcard::Result<Bitfield> {
    postcard::from_bytes(wire)
}
