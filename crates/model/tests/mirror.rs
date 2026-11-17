//! The pairing of two trees by a sync, checked on generated trees against
//! a plain model: a map from each path to what is there.

use model::artifact::{
    Entry, EntryKind, EntryMetadata, MIN_CHUNK_SIZE, PERMISSION_BITS, Timestamp, Tree,
};
use model::mirror::{Plan, Step, plan};
use proptest::collection::vec;
use proptest::prelude::*;
use std::collections::BTreeMap;

/// What is at one path.
type Item = (EntryKind, EntryMetadata);
/// A tree, as a map from each path to what is there. The root is at "".
type Map = BTreeMap<String, Item>;

fn directory(mode: u16, seconds: i64) -> Item {
    let modified = Some(Timestamp { seconds, nanos: 0 });
    (EntryKind::Directory, EntryMetadata { mode, modified })
}

/// Something that can be at the end of a path: a file, a link, or a
/// directory with nothing in it. Few sizes, times and modes, so that two
/// trees often have the same there.
fn leaf() -> impl Strategy<Value = Item> {
    let mode = prop_oneof![Just(0o644u16), Just(0o755u16)];
    let file = (0u64..3, 0i64..3, mode.clone()).prop_map(|(size, seconds, mode)| {
        let modified = Some(Timestamp { seconds, nanos: 0 });
        (EntryKind::File { size }, EntryMetadata { mode, modified })
    });
    // A link has no permissions or time of its own.
    let link = prop_oneof![Just("x"), Just("y")].prop_map(|target| {
        let kind = EntryKind::Symlink {
            target: target.to_owned(),
        };
        let recorded = EntryMetadata {
            mode: PERMISSION_BITS,
            modified: None,
        };
        (kind, recorded)
    });
    let empty = (mode, 0i64..3).prop_map(|(mode, seconds)| directory(mode, seconds));
    prop_oneof![4 => file, 1 => link, 1 => empty]
}

/// A tree of a few levels, from a few names, so that two of them share
/// many paths and differ in what is at them.
fn tree() -> impl Strategy<Value = Map> {
    let name = prop_oneof![Just("a"), Just("b"), Just("c"), Just("a.x")];
    let path = vec(name, 1..4);
    let root = (prop_oneof![Just(0o755u16), Just(0o700u16)], 0i64..3);
    (root, vec((path, leaf(), 0i64..3), 0..10)).prop_map(|((mode, seconds), leaves)| {
        let mut map = Map::new();
        map.insert(String::new(), directory(mode, seconds));
        for (names, item, seconds) in leaves {
            let path = names.join("/");
            // Nothing goes under a file or a link, or where something is.
            let parents: Vec<String> = (1..names.len()).map(|n| names[..n].join("/")).collect();
            let under_a_leaf = parents.iter().any(|parent| {
                map.get(parent)
                    .is_some_and(|(kind, _)| *kind != EntryKind::Directory)
            });
            if under_a_leaf || map.contains_key(&path) {
                continue;
            }
            for parent in parents {
                map.entry(parent)
                    .or_insert_with(|| directory(0o755, seconds));
            }
            map.insert(path, item);
        }
        map
    })
}

fn tree_of(map: &Map) -> Tree {
    let entries = map.iter().map(|(path, (kind, recorded))| {
        let entry = Entry {
            path: path.clone(),
            kind: kind.clone(),
        };
        (entry, *recorded)
    });
    Tree::new("tree", MIN_CHUNK_SIZE, entries.collect()).unwrap()
}

/// The path each step is about, which is the same in both trees.
fn paths(plan: &Plan, source: &Tree, destination: &Tree) -> Vec<String> {
    let path = |tree: &Tree, entry: Option<usize>| Some(tree.entries()[entry?].path.clone());
    plan.steps
        .iter()
        .map(|planned| {
            let ours = path(source, planned.source);
            let theirs = path(destination, planned.destination);
            if let (Some(ours), Some(theirs)) = (&ours, &theirs) {
                assert_eq!(ours, theirs, "two paths were paired");
            }
            ours.or(theirs).expect("a step is about a path")
        })
        .collect()
}

/// What the destination holds once every step of the plan is taken.
fn carried_out(plan: &Plan, paths: &[String], source: &Map, destination: &Map) -> Map {
    let mut after = destination.clone();
    for (planned, path) in plan.steps.iter().zip(paths) {
        match planned.step {
            Step::Keep | Step::Blocked => {}
            Step::Add | Step::Replace => {
                after.insert(path.clone(), source[path].clone());
            }
            Step::Touch => {
                after.get_mut(path).expect("touched what is there").1 = source[path].1;
            }
            Step::Remove => {
                after.remove(path);
            }
        }
    }
    after
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Every path that either tree has gets one step, in the order of the
    /// paths, and the step is about what the two trees have at that path.
    #[test]
    fn every_path_of_either_tree_gets_one_step(
        source in tree(),
        destination in tree(),
        remove in any::<bool>(),
    ) {
        let (ours, theirs) = (tree_of(&source), tree_of(&destination));
        let planned = plan(&ours, Some(&theirs), remove);
        let paths = paths(&planned, &ours, &theirs);

        let mut every: Vec<&String> = source.keys().chain(destination.keys()).collect();
        every.sort();
        every.dedup();
        prop_assert_eq!(paths.iter().collect::<Vec<_>>(), every);
        for (step, path) in planned.steps.iter().zip(&paths) {
            prop_assert_eq!(step.source.is_some(), source.contains_key(path));
            prop_assert_eq!(step.destination.is_some(), destination.contains_key(path));
        }
    }

    /// With leave to remove, the destination ends up holding exactly what
    /// the source holds. Only what the source has not is removed. And a
    /// second sync finds nothing to do.
    #[test]
    fn a_sync_that_may_remove_makes_the_destination_the_source(
        source in tree(),
        destination in tree(),
    ) {
        let (ours, theirs) = (tree_of(&source), tree_of(&destination));
        let planned = plan(&ours, Some(&theirs), true);
        let paths = paths(&planned, &ours, &theirs);
        prop_assert_eq!(planned.count(Step::Blocked), 0);
        for (step, path) in planned.steps.iter().zip(&paths) {
            if step.step == Step::Remove {
                prop_assert!(!source.contains_key(path), "{} was removed", path);
            }
        }

        let after = carried_out(&planned, &paths, &source, &destination);
        prop_assert_eq!(&after, &source);
        let again = plan(&ours, Some(&tree_of(&after)), true);
        prop_assert!(again.is_done(), "{:?}", again);
    }

    /// Without leave to remove, everything the source has is at the
    /// destination afterwards, unless a directory was in its way. What the
    /// destination had of its own is still there. And a second sync finds
    /// nothing more to do.
    #[test]
    fn a_sync_that_may_not_remove_takes_nothing_of_the_destinations_own(
        source in tree(),
        destination in tree(),
    ) {
        let (ours, theirs) = (tree_of(&source), tree_of(&destination));
        let planned = plan(&ours, Some(&theirs), false);
        let paths = paths(&planned, &ours, &theirs);
        prop_assert_eq!(planned.count(Step::Remove), 0);

        let after = carried_out(&planned, &paths, &source, &destination);
        for (step, path) in planned.steps.iter().zip(&paths) {
            match (source.get(path), step.step) {
                (Some(_), Step::Blocked) => {
                    prop_assert_eq!(&destination[path].0, &EntryKind::Directory);
                    prop_assert_eq!(after.get(path), destination.get(path));
                }
                (Some(item), _) => prop_assert_eq!(after.get(path), Some(item)),
                (None, _) => prop_assert_eq!(after.get(path), destination.get(path)),
            }
        }
        // The tree that is left is one a file system can hold.
        let again = plan(&ours, Some(&tree_of(&after)), false);
        for step in &again.steps {
            prop_assert!(matches!(step.step, Step::Keep | Step::Blocked), "{:?}", step);
        }
        prop_assert_eq!(again.count(Step::Blocked), planned.count(Step::Blocked));
    }

    /// With nothing at the destination, everything is added, and the bytes
    /// to bring are the bytes of the source.
    #[test]
    fn with_no_destination_everything_is_added(source in tree(), remove in any::<bool>()) {
        let ours = tree_of(&source);
        let planned = plan(&ours, None, remove);
        prop_assert_eq!(planned.count(Step::Add), source.len());
        prop_assert_eq!(planned.steps.len(), source.len());
        prop_assert_eq!(planned.bytes(&ours), ours.total_size());
        prop_assert_eq!(planned.is_done(), false);
    }
}

/// What tells two files apart, and what does not.
#[test]
fn files_are_compared_by_size_and_time_and_touched_for_their_permissions() {
    let file = |size, seconds: Option<i64>, mode| {
        let modified = seconds.map(|seconds| Timestamp { seconds, nanos: 7 });
        let entry = Entry {
            path: String::new(),
            kind: EntryKind::File { size },
        };
        let recorded = EntryMetadata { mode, modified };
        Tree::new("f", MIN_CHUNK_SIZE, vec![(entry, recorded)]).unwrap()
    };
    let step = |a: &Tree, b: &Tree| plan(a, Some(b), true).steps[0].step;
    let base = file(5, Some(10), 0o644);

    assert_eq!(step(&base, &file(5, Some(10), 0o644)), Step::Keep);
    // Parts of a second are not compared: file systems keep them to
    // different lengths.
    let mut coarser = file(5, Some(10), 0o644);
    coarser = {
        let (entry, mut recorded) = (coarser.entries()[0].clone(), coarser.metadata(0).unwrap());
        recorded.modified = Some(Timestamp {
            seconds: 10,
            nanos: 0,
        });
        Tree::new("f", MIN_CHUNK_SIZE, vec![(entry, recorded)]).unwrap()
    };
    assert_eq!(step(&base, &coarser), Step::Keep);
    assert_eq!(step(&base, &file(6, Some(10), 0o644)), Step::Replace);
    assert_eq!(step(&base, &file(5, Some(11), 0o644)), Step::Replace);
    assert_eq!(step(&base, &file(5, Some(10), 0o755)), Step::Touch);
    // With no time, nothing says a file was not changed, on either side.
    assert_eq!(step(&base, &file(5, None, 0o644)), Step::Replace);
    assert_eq!(step(&file(5, None, 0o644), &base), Step::Replace);
    let timeless = file(5, None, 0o644);
    assert_eq!(step(&timeless, &timeless), Step::Replace);

    // Only the files that are brought are counted.
    assert_eq!(
        plan(&base, Some(&file(6, Some(10), 0o644)), true).bytes(&base),
        5
    );
    assert_eq!(
        plan(&base, Some(&file(5, Some(10), 0o755)), true).bytes(&base),
        0
    );
}

/// A sync that would take away more than half of what is there is one to
/// stop and ask about.
#[test]
fn a_plan_that_removes_more_than_half_says_so() {
    let folder = |names: &[&str]| {
        let recorded = EntryMetadata {
            mode: 0o644,
            modified: Some(Timestamp {
                seconds: 1,
                nanos: 0,
            }),
        };
        let root = Entry {
            path: String::new(),
            kind: EntryKind::Directory,
        };
        let files = names.iter().map(|name| Entry {
            path: (*name).to_owned(),
            kind: EntryKind::File { size: 1 },
        });
        let entries = std::iter::once(root)
            .chain(files)
            .map(|entry| (entry, recorded));
        Tree::new("f", MIN_CHUNK_SIZE, entries.collect()).unwrap()
    };
    let removes_most = |source: &[&str], destination: &[&str], remove| {
        plan(&folder(source), Some(&folder(destination)), remove).removes_most()
    };

    // Half is not more than half.
    assert!(!removes_most(&["a", "b"], &["a", "b", "c", "d"], true));
    assert!(removes_most(&["a"], &["a", "b", "c", "d"], true));
    // A source with nothing in it would empty the destination.
    assert!(removes_most(&[], &["a"], true));
    assert!(removes_most(&[], &["a", "b", "c"], true));
    // Nothing is removed without leave, and nothing from nothing.
    assert!(!removes_most(&[], &["a", "b", "c"], false));
    assert!(!removes_most(&["a"], &[], true));
    assert!(!plan(&folder(&["a"]), None, true).removes_most());
}
