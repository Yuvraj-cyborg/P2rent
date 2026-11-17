use crate::common::Store;
use ::fs::{Trash, as_kept, index, measure, remove, scan, touch};
use model::artifact::{EntryKind, EntryMetadata, MIN_CHUNK_SIZE, PART_SUFFIX, Timestamp, Tree};
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

/// A scan finds the tree that an index records, and reads no file to do
/// it.
#[test]
fn a_scan_finds_the_tree_an_index_records() {
    let tmp = tempfile::tempdir().unwrap();
    let root = sample_tree(tmp.path());
    // A part file is this program's own unfinished work, and is left out.
    fs::write(root.join("nested/unfinished.mux-part"), b"half").unwrap();

    let indexed = index(&root, MIN_CHUNK_SIZE, &|_| {}).unwrap();
    let scanned = scan(&root, MIN_CHUNK_SIZE).unwrap();
    assert_eq!(scanned.tree, **indexed.artifact.tree());
    assert_eq!(scanned.root, indexed.root);
    assert_eq!((scanned.skipped, indexed.skipped), (1, 1));

    // One file is a tree of one entry.
    let file = scan(&root.join("a.bin"), MIN_CHUNK_SIZE).unwrap();
    assert!(file.tree.is_file());
    assert_eq!(file.tree.name(), "a.bin");
    assert_eq!(file.tree.total_size(), (CS * 3 + 17) as u64);

    assert!(scan(&root.join("missing"), MIN_CHUNK_SIZE).is_err());
}

/// A tree is compared with what is on disk as this system keeps it. On
/// Unix that is the tree as it is. Elsewhere only whether a file may be
/// written is kept of its permissions, and nothing of a directory's
/// permissions or time, so a tree from another system is put that way
/// before it is compared.
#[test]
fn a_tree_is_seen_as_this_system_keeps_it() {
    let tmp = tempfile::tempdir().unwrap();
    let root = sample_tree(tmp.path());
    let scanned = scan(&root, MIN_CHUNK_SIZE).unwrap().tree;
    // What a scan finds is already as this system keeps it.
    let kept = as_kept(&scanned).unwrap();
    if cfg!(unix) {
        assert_eq!(kept, scanned);
    }
    assert_eq!(as_kept(&kept).unwrap(), kept);
    assert_eq!(kept.entries(), scanned.entries());

    // A tree from elsewhere, with permissions this system may not keep.
    let from_elsewhere: Vec<_> = scanned
        .entries()
        .iter()
        .cloned()
        .zip(scanned.recorded().iter().map(|recorded| {
            let mut recorded = *recorded;
            recorded.mode = 0o600;
            recorded
        }))
        .collect();
    let from_elsewhere = Tree::new("bundle", MIN_CHUNK_SIZE, from_elsewhere).unwrap();
    let kept = as_kept(&from_elsewhere).unwrap();
    let modes: Vec<u16> = kept.recorded().iter().map(|r| r.mode).collect();
    if cfg!(unix) {
        assert!(modes.iter().all(|&mode| mode == 0o600), "{modes:?}");
    } else {
        // A file that may be written, and a directory.
        assert!(
            modes.iter().all(|&mode| mode == 0o644 || mode == 0o755),
            "{modes:?}"
        );
        let directories = kept.entries().iter().zip(kept.recorded());
        let timed = directories
            .filter(|(entry, _)| !entry.is_file())
            .any(|(_, recorded)| recorded.modified.is_some());
        assert!(!timed, "a directory kept a time");
    }
}

/// What a sync takes away is moved into its trash, under the path it had.
/// A folder goes with what is in it. A file that is to be replaced is
/// given a second name there and stays where it is.
#[test]
fn what_is_taken_away_goes_to_the_trash_under_its_path() {
    let tmp = tempfile::tempdir().unwrap();
    let root = sample_tree(tmp.path());
    let began = Timestamp {
        seconds: 1_700_000_000,
        nanos: 5,
    };
    let trash = Trash::new(&root, began);
    let folder = root.join(".mux-trash").join("1700000000.000000005");
    assert_eq!(trash.folder(), folder);
    assert!(
        !root.join(".mux-trash").exists(),
        "made before it was needed"
    );

    // A folder, with what is in it.
    let deep = fs::read(root.join("nested/deep/c.bin")).unwrap();
    trash.take("nested").unwrap();
    assert!(!root.join("nested").exists());
    assert_eq!(fs::read(folder.join("nested/deep/c.bin")).unwrap(), deep);
    // What went with it is no longer there to take, which is no failure.
    trash.take("nested/b.bin").unwrap();
    trash.take("never was").unwrap();

    // A file that is about to be replaced: it stays, and is in the trash.
    let old = fs::read(root.join("a.bin")).unwrap();
    trash.keep("a.bin").unwrap();
    assert_eq!(fs::read(root.join("a.bin")).unwrap(), old);
    fs::write(root.join("a.bin.new"), b"new").unwrap();
    fs::rename(root.join("a.bin.new"), root.join("a.bin")).unwrap();
    assert_eq!(fs::read(root.join("a.bin")).unwrap(), b"new");
    assert_eq!(fs::read(folder.join("a.bin")).unwrap(), old);

    // A scan does not look in the trash: it is not what the folder holds.
    let scanned = scan(&root, MIN_CHUNK_SIZE).unwrap();
    let paths: Vec<&str> = scanned
        .tree
        .entries()
        .iter()
        .map(|e| e.path.as_str())
        .collect();
    assert_eq!(paths, ["", "a.bin", "empty"]);
    assert_eq!(scanned.skipped, 0);
    // Only at the top: a folder of that name further in is a folder.
    fs::create_dir_all(root.join("sub/.mux-trash")).unwrap();
    let scanned = scan(&root, MIN_CHUNK_SIZE).unwrap();
    assert!(
        scanned
            .tree
            .entries()
            .iter()
            .any(|e| e.path == "sub/.mux-trash")
    );
}

/// For a sync that keeps no trash, and for one that only sets permissions.
#[test]
fn a_thing_is_removed_for_good_and_a_file_is_given_its_permissions() {
    let tmp = tempfile::tempdir().unwrap();
    let root = sample_tree(tmp.path());
    remove(&root.join("nested")).unwrap();
    remove(&root.join("empty")).unwrap();
    remove(&root.join("never was")).unwrap();
    assert!(!root.join("nested").exists() && !root.join("empty").exists());
    assert!(root.join("a.bin").exists());

    let before = fs::metadata(root.join("a.bin"))
        .unwrap()
        .modified()
        .unwrap();
    let recorded = EntryMetadata {
        mode: 0o444,
        modified: None,
    };
    touch(&root.join("a.bin"), recorded).unwrap();
    let after = fs::metadata(root.join("a.bin")).unwrap();
    assert!(after.permissions().readonly());
    assert_eq!(after.modified().unwrap(), before, "its time was changed");
    assert!(touch(&root.join("never was"), recorded).is_err());
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

    assert!(!first.artifact.is_file());
    assert_eq!(first.artifact.name(), "bundle");
    let entries = first.artifact.entries();
    let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
    // The root, then everything under it, directories included.
    assert_eq!(
        paths,
        [
            "",
            "a.bin",
            "empty",
            "nested",
            "nested/b.bin",
            "nested/deep",
            "nested/deep/c.bin"
        ]
    );
    let directories: Vec<&str> = entries
        .iter()
        .filter(|e| e.kind == EntryKind::Directory)
        .map(|e| e.path.as_str())
        .collect();
    assert_eq!(directories, ["", "nested", "nested/deep"]);
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
    assert!(before.artifact.is_file());
    assert_eq!(before.artifact.entries().len(), 1);

    fs::write(&file, bytes(CS * 2, 9)).unwrap();
    let after = index(&file, MIN_CHUNK_SIZE, &|_| {}).unwrap();
    assert_ne!(before.artifact.id(), after.artifact.id());
}

/// A destination next to the sample tree, the artifact, and a store over
/// the original files to copy chunks from.
fn sample(tmp: &std::path::Path) -> (Arc<model::artifact::Artifact>, Store, PathBuf) {
    let root = sample_tree(tmp);
    let indexed = index(&root, MIN_CHUNK_SIZE, &|_| {}).unwrap();
    let artifact = Arc::new(indexed.artifact);
    let source = Store::whole(artifact.clone(), indexed.root);
    (artifact, source, tmp.join("out"))
}

fn resume(artifact: &Arc<model::artifact::Artifact>, out: &std::path::Path) -> Store {
    Store::resume(artifact.clone(), out).unwrap()
}

fn copy(source: &Store, dest: &Store, chunk: u64) {
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
    dest.seal().unwrap();
    dest.seal().unwrap();
    assert!(resume(&artifact, &out).is_complete());

    for entry in artifact.entries().iter().filter(|entry| entry.is_file()) {
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
    assert!(Store::resume(artifact, &out).is_err());
}

/// Copies every chunk of `source` into a new store under `out`, and
/// finishes it.
fn transfer(
    artifact: &Arc<model::artifact::Artifact>,
    source: &Store,
    out: &std::path::Path,
) -> Store {
    let dest = resume(artifact, out);
    for chunk in 0..artifact.chunk_count() {
        copy(source, &dest, chunk);
    }
    dest.seal().unwrap();
    dest
}

#[test]
fn a_tree_arrives_with_its_empty_directories() {
    let tmp = tempfile::tempdir().unwrap();
    let root = sample_tree(tmp.path());
    fs::create_dir_all(root.join("hollow/inner")).unwrap();

    let indexed = index(&root, MIN_CHUNK_SIZE, &|_| {}).unwrap();
    let artifact = Arc::new(indexed.artifact);
    let source = Store::whole(artifact.clone(), indexed.root);
    let out = tmp.path().join("out");
    transfer(&artifact, &source, &out);

    assert!(out.join("bundle/hollow/inner").is_dir());
    assert!(out.join("bundle/nested/deep").is_dir());
}

#[cfg(unix)]
#[test]
fn a_tree_arrives_with_its_links_permissions_and_times() {
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, SystemTime};
    let tmp = tempfile::tempdir().unwrap();
    let root = sample_tree(tmp.path());
    let chmod = |path: &str, mode| {
        fs::set_permissions(root.join(path), fs::Permissions::from_mode(mode)).unwrap();
    };
    let touch = |path: &str, seconds| {
        let time = SystemTime::UNIX_EPOCH + Duration::new(seconds, 123_456_789);
        fs::File::open(root.join(path))
            .unwrap()
            .set_modified(time)
            .unwrap();
    };
    std::os::unix::fs::symlink("nested/b.bin", root.join("latest")).unwrap();
    std::os::unix::fs::symlink("../a.bin", root.join("nested/up")).unwrap();
    std::os::unix::fs::symlink("nowhere", root.join("dangling")).unwrap();
    fs::write(root.join("run.sh"), b"#!/bin/sh\n").unwrap();
    chmod("run.sh", 0o750);
    chmod("a.bin", 0o600);
    chmod("empty", 0o444);
    touch("a.bin", 1_000_000_000);
    touch("empty", 86_400);
    // Directories last: changing what is in one changes its time.
    chmod("nested/deep", 0o700);
    touch("nested/deep", 1_234_567_890);
    touch("nested", 1_111_111_111);

    let indexed = index(&root, MIN_CHUNK_SIZE, &|_| {}).unwrap();
    assert_eq!(indexed.skipped, 0);
    let artifact = Arc::new(indexed.artifact);
    let source = Store::whole(artifact.clone(), indexed.root);
    let out = tmp.path().join("out");
    let dest = transfer(&artifact, &source, &out);
    assert_eq!(dest.skipped_links(), 0);

    let arrived = out.join("bundle");
    for link in ["latest", "nested/up", "dangling"] {
        assert_eq!(
            fs::read_link(arrived.join(link)).unwrap(),
            fs::read_link(root.join(link)).unwrap(),
            "{link}"
        );
    }
    assert_eq!(
        fs::read(arrived.join("latest")).unwrap(),
        fs::read(root.join("nested/b.bin")).unwrap()
    );
    for path in [
        "run.sh",
        "a.bin",
        "empty",
        "nested/b.bin",
        "nested/deep",
        "nested",
    ] {
        let was = fs::metadata(root.join(path)).unwrap();
        let is = fs::metadata(arrived.join(path)).unwrap();
        assert_eq!(
            is.permissions().mode() & 0o777,
            was.permissions().mode() & 0o777,
            "permissions of {path}"
        );
        assert_eq!(
            is.modified().unwrap(),
            was.modified().unwrap(),
            "time of {path}"
        );
    }

    // Opening it again changes nothing, and replaces no link.
    let again = resume(&artifact, &out);
    assert!(again.is_complete());
    again.seal().unwrap();
    assert_eq!(
        fs::metadata(arrived.join("nested"))
            .unwrap()
            .modified()
            .unwrap(),
        fs::metadata(root.join("nested"))
            .unwrap()
            .modified()
            .unwrap()
    );
}

#[cfg(unix)]
#[test]
fn indexing_leaves_out_a_link_that_points_outside_the_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let root = sample_tree(tmp.path());
    let before = index(&root, MIN_CHUNK_SIZE, &|_| {}).unwrap();

    std::os::unix::fs::symlink("/etc/passwd", root.join("absolute")).unwrap();
    std::os::unix::fs::symlink("../outside", root.join("above")).unwrap();
    std::os::unix::fs::symlink("../../outside", root.join("nested/above")).unwrap();

    let after = index(&root, MIN_CHUNK_SIZE, &|_| {}).unwrap();
    assert_eq!(after.skipped, 3);
    assert_eq!(after.artifact.content_id(), before.artifact.content_id());
}

/// Nothing is written through a link: one standing where a directory of
/// the artifact belongs is refused, and one standing where a file
/// belongs is replaced, not followed.
#[cfg(unix)]
#[test]
fn a_link_in_the_way_is_never_written_through() {
    let tmp = tempfile::tempdir().unwrap();
    let (artifact, source, out) = sample(tmp.path());
    let elsewhere = tmp.path().join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    fs::write(elsewhere.join("victim"), b"untouched").unwrap();

    fs::create_dir_all(out.join("bundle")).unwrap();
    std::os::unix::fs::symlink(&elsewhere, out.join("bundle/nested")).unwrap();
    let refused = Store::resume(artifact.clone(), &out).unwrap_err();
    assert!(refused.to_string().contains("not a directory"), "{refused}");
    assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 1);

    fs::remove_file(out.join("bundle/nested")).unwrap();
    std::os::unix::fs::symlink(elsewhere.join("victim"), out.join("bundle/a.bin")).unwrap();
    transfer(&artifact, &source, &out);
    assert_eq!(fs::read(elsewhere.join("victim")).unwrap(), b"untouched");
    assert!(
        !fs::symlink_metadata(out.join("bundle/a.bin"))
            .unwrap()
            .is_symlink()
    );
}

/// Two names that this file system treats as one cannot both be stored.
/// Where it tells them apart, both arrive.
#[test]
fn names_the_file_system_cannot_tell_apart_are_refused() {
    use model::artifact::{Artifact, Entry, EntryMetadata, Hash, Manifest};
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("probe"), b"").unwrap();
    let ignores_case = tmp.path().join("PROBE").exists();

    let plain = EntryMetadata {
        mode: 0o644,
        modified: None,
    };
    let entry = |path: &str, kind| {
        (
            Entry {
                path: path.into(),
                kind,
            },
            plain,
        )
    };
    let contents = bytes(10, 1);
    let manifest = Manifest::new(
        "tree",
        MIN_CHUNK_SIZE,
        vec![
            entry("", EntryKind::Directory),
            entry("README", EntryKind::File { size: 10 }),
            entry("readme", EntryKind::File { size: 10 }),
        ],
        vec![Hash::of(&contents); 2],
    );
    let artifact = Arc::new(Artifact::new(manifest).unwrap());
    let opened = Store::resume(artifact, &tmp.path().join("out"));

    if ignores_case {
        let refused = opened.unwrap_err().to_string();
        assert!(refused.contains("the same name"), "{refused}");
    } else {
        let dest = opened.unwrap();
        assert!(dest.write_chunk(0, &contents).unwrap());
        assert!(dest.write_chunk(1, &contents).unwrap());
        assert!(dest.is_complete());
    }
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
    assert_eq!(after.artifact.content_id(), before.artifact.content_id());
    assert_eq!(after.skipped, before.skipped + 2);
}
