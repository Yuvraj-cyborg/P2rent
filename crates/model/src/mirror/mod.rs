//! Making a destination hold what a source holds.
//!
//! [`plan`] goes through two trees side by side. For each path it finds
//! what each tree has there, and asks [`step::decide`] what to do about
//! it. The decision is proven in Lean. This pairing is not: it is checked
//! by tests against a plain model.
//!
//! Nothing here touches a disk. A plan is carried out elsewhere.

pub mod step;

pub use step::{Kind, Pair, Step};

use crate::artifact::path::precedes;
use crate::artifact::{Entry, EntryKind, EntryMetadata, Tree};
use alloc::vec::Vec;

/// What to do about one path, and which entry of each tree it is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Planned {
    /// What to do.
    pub step: Step,
    /// What the source has there, as its place among the source's entries.
    pub source: Option<usize>,
    /// What the destination has there, as its place among its entries.
    pub destination: Option<usize>,
}

/// What to do so that a destination holds what a source holds: one step for
/// each path that either of them has, in the order of the paths.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    /// The steps.
    pub steps: Vec<Planned>,
}

impl Plan {
    /// How many paths the plan has `step` for.
    pub fn count(&self, step: Step) -> usize {
        self.steps
            .iter()
            .filter(|planned| planned.step == step)
            .count()
    }

    /// The bytes of every file that the plan puts at the destination: the
    /// files of `source` that are added, or that replace something.
    pub fn bytes(&self, source: &Tree) -> u64 {
        let brought = self
            .steps
            .iter()
            .filter(|planned| matches!(planned.step, Step::Add | Step::Replace));
        brought
            .filter_map(|planned| source.entries().get(planned.source?))
            .map(Entry::size)
            .sum()
    }

    /// Whether the destination already holds what the source holds, as far
    /// as the plan goes: there is nothing to do but keep.
    pub fn is_done(&self) -> bool {
        self.steps.iter().all(|planned| planned.step == Step::Keep)
    }
}

/// Works out what to do so that `destination` holds what `source` holds.
/// With no destination, everything is to be added.
///
/// Two files are taken to hold the same if they have the same size and
/// were last changed in the same second. Parts of a second are not
/// compared: file systems keep them to different lengths, and a copy on a
/// coarser one would never be found to match. This is the check `rsync`
/// makes by default. A file with no time recorded is never taken to hold
/// the same: nothing says it was not changed.
///
/// With `remove` unset, nothing that the destination has and the source has
/// not is taken away.
pub fn plan(source: &Tree, destination: Option<&Tree>, remove: bool) -> Plan {
    let (ours, of_ours) = (source.entries(), source.recorded());
    let theirs = destination.map_or(&[][..], Tree::entries);
    let of_theirs = destination.map_or(&[][..], Tree::recorded);
    let mut steps = Vec::with_capacity(ours.len().max(theirs.len()));
    let (mut at, mut there) = (0, 0);
    // Both lists are in the order of their paths, so the two are gone
    // through together, and a path that both have is met in both at once.
    while at < ours.len() || there < theirs.len() {
        let pair = match (ours.get(at), theirs.get(there)) {
            (Some(a), Some(b)) if precedes(a.path.as_bytes(), b.path.as_bytes()) => {
                (Some(at), None)
            }
            (Some(a), Some(b)) if precedes(b.path.as_bytes(), a.path.as_bytes()) => {
                (None, Some(there))
            }
            (Some(_), Some(_)) => (Some(at), Some(there)),
            (Some(_), None) => (Some(at), None),
            (None, _) => (None, Some(there)),
        };
        let found = match pair {
            (Some(a), Some(b)) => Pair::Both {
                source: kind_of(&ours[a]),
                destination: kind_of(&theirs[b]),
                same_content: same_content((&ours[a], of_ours[a]), (&theirs[b], of_theirs[b])),
                same_manner: same_manner((&ours[a], of_ours[a]), (&theirs[b], of_theirs[b])),
            },
            (Some(_), None) => Pair::OnlySource,
            (None, _) => Pair::OnlyDestination,
        };
        steps.push(Planned {
            step: step::decide(found, remove),
            source: pair.0,
            destination: pair.1,
        });
        at += usize::from(pair.0.is_some());
        there += usize::from(pair.1.is_some());
    }
    Plan { steps }
}

/// An entry, with what is recorded of it besides what it holds.
type Recorded<'a> = (&'a Entry, EntryMetadata);

fn kind_of(entry: &Entry) -> Kind {
    match entry.kind {
        EntryKind::File { .. } => Kind::File,
        EntryKind::Directory => Kind::Directory,
        EntryKind::Symlink { .. } => Kind::Link,
    }
}

/// Whether two entries of one kind hold the same, as far as can be told
/// without reading them.
fn same_content(a: Recorded<'_>, b: Recorded<'_>) -> bool {
    match (&a.0.kind, &b.0.kind) {
        (EntryKind::File { size: ours }, EntryKind::File { size: theirs }) => {
            ours == theirs && second_of(a.1).is_some() && second_of(a.1) == second_of(b.1)
        }
        (EntryKind::Symlink { target: ours }, EntryKind::Symlink { target: theirs }) => {
            ours == theirs
        }
        // A directory holds nothing of its own. And of two entries of
        // different kinds the question is not asked.
        _ => true,
    }
}

/// The second in which an entry was last changed, if that is recorded.
fn second_of(recorded: EntryMetadata) -> Option<i64> {
    Some(recorded.modified?.seconds)
}

/// Whether two entries have the same permissions, and for a directory the
/// same time, to the second. A file's time is part of what tells whether
/// it holds the same. A link has neither of its own.
fn same_manner(a: Recorded<'_>, b: Recorded<'_>) -> bool {
    match a.0.kind {
        EntryKind::File { .. } => a.1.mode == b.1.mode,
        EntryKind::Directory => a.1.mode == b.1.mode && second_of(a.1) == second_of(b.1),
        EntryKind::Symlink { .. } => true,
    }
}
