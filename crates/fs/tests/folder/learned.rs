//! A tree whose digests are not known beforehand, in a local folder: what
//! is learned is noted at the end of the part file, and cut off when the
//! file takes its name.

use crate::common::Store;
use ::fs::index;
use model::artifact::{MIN_CHUNK_SIZE, PART_SUFFIX};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const CHUNK: usize = MIN_CHUNK_SIZE as usize;

/// A file of four chunks and a bit, with bytes that do not repeat, and a
/// store over it to copy chunks from.
fn source(tmp: &Path) -> (Vec<u8>, Store) {
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let bytes: Vec<u8> = (0..CHUNK * 4 + 100)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[0]
        })
        .collect();
    let path = tmp.join("image.bin");
    fs::write(&path, &bytes).unwrap();
    let indexed = index(&path, MIN_CHUNK_SIZE, &|_| {}).unwrap();
    let whole = Store::whole(Arc::new(indexed.artifact), indexed.root);
    (bytes, whole)
}

fn part(out: &Path) -> PathBuf {
    out.join(format!("image.bin{PART_SUFFIX}"))
}

fn copy(from: &Store, to: &Store, chunk: u64) {
    assert!(
        to.write_chunk(chunk, &from.read_chunk(chunk).unwrap())
            .unwrap()
    );
}

#[test]
fn what_is_learned_is_noted_after_the_bytes_and_cut_off_at_the_end() {
    let tmp = tempfile::tempdir().unwrap();
    let (bytes, whole) = source(tmp.path());
    let artifact = whole.artifact().unwrap();
    let out = tmp.path().join("out");

    let dest = Store::learn(&artifact, &out, b"\"v1\"").unwrap();
    assert_eq!(dest.held().count_ones(), 0);
    assert_eq!(dest.artifact(), None, "no digest is known yet");
    for chunk in [3, 0, 1] {
        copy(&whole, &dest, chunk);
    }
    // The part file is the file's bytes and then the notes.
    let size = bytes.len() as u64;
    assert!(fs::metadata(part(&out)).unwrap().len() > size);
    assert!(!out.join("image.bin").exists());

    // Opened again, it holds what it noted and nothing else.
    let dest = Store::learn(&artifact, &out, b"\"v1\"").unwrap();
    assert_eq!(dest.held().count_ones(), 3);
    assert!(!dest.has(2) && !dest.has(4));
    for chunk in [2, 4] {
        copy(&whole, &dest, chunk);
    }
    assert!(dest.is_complete());
    dest.seal().unwrap();

    // The file has its name, its own bytes and no more, and what was
    // learned is the artifact that those bytes are.
    assert!(!part(&out).exists());
    assert_eq!(fs::read(out.join("image.bin")).unwrap(), bytes);
    assert_eq!(dest.artifact(), Some(artifact));
}

#[test]
fn a_part_file_of_another_version_is_started_again() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, whole) = source(tmp.path());
    let artifact = whole.artifact().unwrap();
    let out = tmp.path().join("out");

    let dest = Store::learn(&artifact, &out, b"\"v1\"").unwrap();
    copy(&whole, &dest, 0);
    copy(&whole, &dest, 1);

    for other in [&b"\"v2\""[..], b""] {
        let dest = Store::learn(&artifact, &out, other).unwrap();
        assert_eq!(dest.held().count_ones(), 0, "kept from another version");
    }
}

#[test]
fn a_noted_chunk_that_no_longer_has_its_digest_is_not_held() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, whole) = source(tmp.path());
    let artifact = whole.artifact().unwrap();
    let out = tmp.path().join("out");

    let dest = Store::learn(&artifact, &out, b"\"v1\"").unwrap();
    for chunk in 0..3 {
        copy(&whole, &dest, chunk);
    }
    // Damage the second chunk where it lies in the part file.
    let mut on_disk = fs::read(part(&out)).unwrap();
    on_disk[CHUNK + 10] ^= 0xff;
    fs::write(part(&out), on_disk).unwrap();

    let dest = Store::learn(&artifact, &out, b"\"v1\"").unwrap();
    assert!(dest.has(0) && !dest.has(1) && dest.has(2));
}

/// A part file left by a transfer that knew its digests has no notes. A
/// transfer that does not know them cannot tell what its bytes are, and
/// keeps none of them.
#[test]
fn a_part_file_with_no_notes_is_not_believed() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, whole) = source(tmp.path());
    let artifact = whole.artifact().unwrap();
    let out = tmp.path().join("out");

    let known = Store::resume(artifact.clone(), &out).unwrap();
    copy(&whole, &known, 0);
    assert_eq!(
        Store::resume(artifact.clone(), &out)
            .unwrap()
            .held()
            .count_ones(),
        1
    );

    let learning = Store::learn(&artifact, &out, b"\"v1\"").unwrap();
    assert_eq!(learning.held().count_ones(), 0);
}

/// A part file that has every chunk is put in place as the folder is made
/// ready, and so is a file with no bytes. Neither has anything left to
/// keep notes beside.
#[test]
fn a_file_that_is_finished_on_opening_needs_no_notes() {
    let tmp = tempfile::tempdir().unwrap();
    let (bytes, whole) = source(tmp.path());
    let artifact = whole.artifact().unwrap();
    let out = tmp.path().join("out");

    // Every chunk is written, but the store is let go of before the last
    // one, so that the file is whole in its part file and not renamed.
    let dest = Store::learn(&artifact, &out, b"\"v1\"").unwrap();
    for chunk in 0..artifact.chunk_count() - 1 {
        copy(&whole, &dest, chunk);
    }
    let last = artifact.chunk_count() - 1;
    let at = artifact.locate(last).unwrap();
    let mut on_disk = fs::read(part(&out)).unwrap();
    let chunk = whole.read_chunk(last).unwrap();
    let start = usize::try_from(at.offset).unwrap();
    on_disk[start..start + chunk.len()].copy_from_slice(&chunk);
    fs::write(part(&out), &on_disk).unwrap();
    // Note its digest too, as the store would have.
    let notes = model::notes::Notes::new(bytes.len() as u64, MIN_CHUNK_SIZE, b"\"v1\"").unwrap();
    let (offset, slot) = notes.slot(last, model::artifact::Hash::of(&chunk)).unwrap();
    let mut on_disk = fs::read(part(&out)).unwrap();
    let start = bytes.len() + usize::try_from(offset).unwrap();
    on_disk.resize(on_disk.len().max(start + slot.len()), 0);
    on_disk[start..start + slot.len()].copy_from_slice(&slot);
    fs::write(part(&out), &on_disk).unwrap();

    let dest = Store::learn(&artifact, &out, b"\"v1\"").unwrap();
    assert!(dest.is_complete());
    assert!(!part(&out).exists());
    assert_eq!(fs::read(out.join("image.bin")).unwrap(), bytes);
}

#[test]
fn a_file_with_no_bytes_is_there_from_the_start() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("empty.bin");
    fs::write(&path, b"").unwrap();
    let indexed = index(&path, MIN_CHUNK_SIZE, &|_| {}).unwrap();
    let artifact = Arc::new(indexed.artifact);
    let out = tmp.path().join("out");

    let dest = Store::learn(&artifact, &out, b"\"v1\"").unwrap();
    assert!(dest.is_complete());
    assert_eq!(fs::read(out.join("empty.bin")).unwrap(), b"");
}
