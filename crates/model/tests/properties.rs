//! Properties of the data model, checked on generated inputs.

use model::access::{Guests, Secret};
use model::artifact::{
    Artifact, Bitfield, Entry, EntryKind, EntryMetadata, Hash, MIN_CHUNK_SIZE, Manifest, Timestamp,
};
use model::hex;
use model::rolling::Rolling;
use model::run;
use model::schedule::{ChunkState, Outcome, Owner, Schedule};
use model::search::{Searched, Wanted};
use model::ticket::NodeId;
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
        owner: Owner,
        available: BTreeSet<u64>,
        cursor: u64,
    },
    Settle {
        index: u64,
        owner: Owner,
        outcome: Outcome,
    },
}

/// Three fetchers are enough for one to settle what another claimed.
fn steps(len: u64) -> impl Strategy<Value = Vec<Step>> {
    let owner = 0..3u32;
    let outcome = prop_oneof![Just(Outcome::Verified), Just(Outcome::Failed)];
    vec(
        prop_oneof![
            (owner.clone(), btree_set(0..len.max(1), 0..16), any::<u64>()).prop_map(
                |(owner, available, cursor)| Step::Claim {
                    owner,
                    available,
                    cursor
                }
            ),
            (0..len + 2, owner, outcome).prop_map(|(index, owner, outcome)| Step::Settle {
                index,
                owner,
                outcome
            }),
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
        prop_assert_eq!(&decoded, &artifact);
        // And what was decoded says what the manifest said.
        prop_assert_eq!(decoded.name(), &manifest.metadata.name);
        prop_assert_eq!(decoded.chunk_size(), manifest.content.chunk_size);
        prop_assert_eq!(decoded.entries(), &manifest.content.entries[..]);
        let metadata: Vec<_> = (0..manifest.metadata.entries.len())
            .map(|entry| decoded.metadata(entry).unwrap())
            .collect();
        prop_assert_eq!(metadata, manifest.metadata.entries);
        let digests: Vec<_> = (0..decoded.chunk_count())
            .map(|index| decoded.chunk_hash(index).unwrap())
            .collect();
        prop_assert_eq!(digests, manifest.content.chunks);
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
                Step::Claim { owner, available, mut cursor } => {
                    let available: BTreeSet<u64> =
                        available.into_iter().filter(|&i| i < len).collect();
                    // The first candidate at or after the cursor, wrapping.
                    let expected = (0..len)
                        .map(|step| (cursor % len.max(1) + step) % len.max(1))
                        .find(|&i| {
                            model[usize::try_from(i).unwrap()] == ChunkState::Missing
                                && available.contains(&i)
                        });
                    let claimed = schedule.claim(owner, &bitfield(len, &available), &mut cursor);
                    prop_assert_eq!(claimed, expected);
                    if let Some(index) = claimed {
                        model[usize::try_from(index).unwrap()] = ChunkState::InFlight(owner);
                        prop_assert_eq!(cursor, (index + 1) % len);
                    }
                }
                Step::Settle { index, owner, outcome } => {
                    let slot = usize::try_from(index).unwrap();
                    // Only the fetcher of a chunk settles it, and only a
                    // verified chunk is done.
                    let mine = model.get(slot) == Some(&ChunkState::InFlight(owner));
                    prop_assert_eq!(schedule.settle(index, owner, outcome), mine);
                    if mine {
                        model[slot] = match outcome {
                            Outcome::Verified => ChunkState::Done,
                            Outcome::Failed => ChunkState::Missing,
                        };
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
/// A length of time in microseconds: of any size, or of one a transfer
/// meets, so that both the edges and the middle are tried.
fn micros() -> impl Strategy<Value = u64> {
    prop_oneof![any::<u64>(), 0u64..100_000_000, 0u64..2_000]
}

proptest! {
    /// The faster a stream is, the less it has to gain to take over from
    /// another, and never more than a connection is worth. The bounded
    /// proof of this in `model::run` covers times of up to 65 thousandths
    /// of a second: this tries times of every size.
    #[test]
    fn a_faster_stream_never_has_to_gain_more(
        each in micros(),
        one in micros(),
        other in micros(),
    ) {
        let (faster, slower) = (one.min(other), one.max(other));
        prop_assert!(run::worth(each, faster) <= run::worth(each, slower));
        prop_assert!(run::worth(each, slower) <= run::WORTH_MICROS);
        if faster >= each {
            prop_assert_eq!(run::worth(each, faster), run::WORTH_MICROS);
        }
    }

    /// What is taken over is within what is left, and a chunk is fetched
    /// a second time only by a stream that would have it sooner.
    #[test]
    fn what_is_taken_over_is_there_to_take(
        left in any::<u64>(),
        each in micros(),
        mine in micros(),
        waited in micros(),
    ) {
        prop_assert!(run::spare(left, each, mine) <= left);
        if run::relieve(each, waited, mine) {
            let remaining = if waited < each { each - waited } else { waited };
            prop_assert!(mine > 0 && mine < remaining);
        }
    }
}

/// An older version of a file and a newer one: the older with a run of
/// bytes put in, taken out or written over somewhere. Bytes of few values,
/// so that the same run turns up in several places.
fn two_versions() -> impl Strategy<Value = (Vec<u8>, Vec<u8>)> {
    let old = vec(0u8..4, 0..300);
    (
        old,
        any::<prop::sample::Index>(),
        vec(0u8..4, 0..40),
        0usize..40,
    )
        .prop_map(|(old, at, put_in, taken_out)| {
            let at = at.index(old.len() + 1);
            let end = (at + taken_out).min(old.len());
            let mut new = old[..at].to_vec();
            new.extend_from_slice(&put_in);
            new.extend_from_slice(&old[end..]);
            (old, new)
        })
}

proptest! {
    /// Whatever was done to a file, a search of the older version finds
    /// every whole chunk of the newer one that the older holds anywhere,
    /// says rightly where, and finds nothing else. And it finds the same
    /// whatever the size of the blocks the file is read in.
    #[test]
    fn a_search_finds_every_chunk_the_old_file_holds(
        (old, new) in two_versions(),
        len in 1u32..24,
        block in 1usize..80,
    ) {
        let size = usize::try_from(len).unwrap();
        let chunks: Vec<&[u8]> = new.chunks_exact(size).collect();
        let mut wanted = Wanted::new(len, u64::MAX);
        for (index, chunk) in (0u64..).zip(&chunks) {
            wanted.add(index, Rolling::of(chunk).value(), Hash::of(chunk));
        }
        // Block by block, each beginning with what the one before kept.
        let block = block.max(size);
        let mut finds: Vec<(usize, usize)> = Vec::new();
        let (mut from, mut end) = (0, block.min(old.len()));
        loop {
            let ended = wanted.search(&old[from..end], &mut |index, at| {
                finds.push((usize::try_from(index).unwrap(), from + at));
            });
            match ended {
                Searched::On(kept) if end < old.len() => {
                    from += kept;
                    end = (end + block).min(old.len());
                }
                _ => break,
            }
        }

        for &(index, at) in &finds {
            prop_assert_eq!(&old[at..at + size], chunks[index]);
        }
        let found: BTreeSet<usize> = finds.iter().map(|&(index, _)| index).collect();
        prop_assert_eq!(found.len(), finds.len(), "a chunk was found twice");
        for (index, chunk) in chunks.iter().enumerate() {
            let held = old.windows(size).any(|window| window == *chunk);
            prop_assert_eq!(found.contains(&index), held, "chunk {}", index);
        }
    }
}

fn postcard_pair(len: u64, bytes: &[u8]) -> Vec<u8> {
    postcard::to_allocvec(&(len, bytes)).unwrap()
}

fn decode_bitfield(wire: &[u8]) -> postcard::Result<Bitfield> {
    postcard::from_bytes(wire)
}

/// A machine told from the others by one byte.
fn machine(byte: u8) -> NodeId {
    NodeId::from_bytes([byte; 32])
}

proptest! {
    /// Who is let in, compared with a set that is filled as machines come.
    #[test]
    fn the_first_machines_to_ask_are_let_in_and_no_other(
        room in 0usize..6,
        asking in vec(0u8..8, 0..40),
    ) {
        let mut guests = Guests::first(room);
        let mut inside = BTreeSet::new();
        for byte in asking {
            let expected = inside.contains(&byte) || inside.len() < room;
            prop_assert_eq!(guests.admit(&machine(byte)), expected);
            if expected {
                inside.insert(byte);
            }
        }
        prop_assert!(inside.len() <= room);
    }

    #[test]
    fn the_machines_that_are_named_are_let_in_and_no_other(
        named in btree_set(0u8..8, 0..5),
        asking in vec(0u8..8, 0..40),
    ) {
        let mut guests = Guests::only(named.iter().copied().map(machine).collect());
        for byte in asking {
            prop_assert_eq!(guests.admit(&machine(byte)), named.contains(&byte));
        }
    }

    /// A proof is good for the secret, the server and the client it was
    /// made for, and for nothing that differs in any of the three.
    #[test]
    fn a_proof_is_good_for_what_it_was_made_for_alone(
        made in (any::<[u8; 16]>(), any::<[u8; 32]>(), any::<[u8; 32]>()),
        shown in (any::<[u8; 16]>(), any::<[u8; 32]>(), any::<[u8; 32]>()),
    ) {
        let proof = |(secret, server, client): ([u8; 16], [u8; 32], [u8; 32])| {
            let (server, client) = (NodeId::from_bytes(server), NodeId::from_bytes(client));
            Secret::from_bytes(secret).proof(&server, &client)
        };
        prop_assert_eq!(proof(made) == proof(shown), made == shown);
        prop_assert_eq!(proof(made), proof(made));
    }
}
