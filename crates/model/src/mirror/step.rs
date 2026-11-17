//! What a sync does about one path.
//!
//! A sync makes a destination hold what a source holds. For each path it
//! looks at what the two have there and takes one step. This module is that
//! decision and nothing else. It is translated to Lean and proven (see
//! `proofs/`), so it is written plainly, and it knows nothing of paths,
//! sizes or times: whoever calls it has compared those already.

/// What kind of thing is at a path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A regular file.
    File,
    /// A directory.
    Directory,
    /// A symbolic link.
    Link,
}

/// What the source and the destination have at one path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pair {
    /// The source has something there and the destination has nothing.
    OnlySource,
    /// The destination has something there and the source has nothing.
    OnlyDestination,
    /// Both have something there.
    Both {
        /// What the source has.
        source: Kind,
        /// What the destination has.
        destination: Kind,
        /// Whether the two hold the same: for files the same bytes, as far
        /// as the caller can tell, and for links the same target.
        /// Directories hold nothing of their own, and always do.
        same_content: bool,
        /// Whether the two have the same permissions and, where it is
        /// kept, the same time.
        same_manner: bool,
    },
}

/// What to do about one path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Nothing. The destination has what the source has, or has something
    /// of its own that is not to be removed.
    Keep,
    /// Put there what the source has. The destination has nothing there.
    Add,
    /// Take away what the destination has and put there what the source
    /// has.
    Replace,
    /// Give what the destination has the permissions and the time that
    /// the source's has. What it holds is the same already.
    Touch,
    /// Take away what the destination has. The source has nothing there.
    Remove,
    /// Nothing, though the two differ: the destination has a directory
    /// where the source has something else, and nothing is to be removed.
    /// Putting the source's there would remove what the directory holds.
    Blocked,
}

/// Whether two kinds are the same kind.
fn same_kind(a: Kind, b: Kind) -> bool {
    matches!(
        (a, b),
        (Kind::File, Kind::File) | (Kind::Directory, Kind::Directory) | (Kind::Link, Kind::Link)
    )
}

fn is_directory(kind: Kind) -> bool {
    matches!(kind, Kind::Directory)
}

/// The step to take about a path, given what the two have there. With
/// `remove` unset, nothing that the destination has and the source has not
/// is taken away.
pub fn decide(pair: Pair, remove: bool) -> Step {
    match pair {
        Pair::OnlySource => Step::Add,
        Pair::OnlyDestination => {
            if remove {
                Step::Remove
            } else {
                Step::Keep
            }
        }
        Pair::Both {
            source,
            destination,
            same_content,
            same_manner,
        } => {
            if same_kind(source, destination) {
                if !same_content {
                    Step::Replace
                } else if !same_manner {
                    Step::Touch
                } else {
                    Step::Keep
                }
            } else if is_directory(destination) && !remove {
                Step::Blocked
            } else {
                Step::Replace
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KINDS: [Kind; 3] = [Kind::File, Kind::Directory, Kind::Link];

    fn both(source: Kind, destination: Kind, same_content: bool, same_manner: bool) -> Pair {
        Pair::Both {
            source,
            destination,
            same_content,
            same_manner,
        }
    }

    #[test]
    fn what_only_one_side_has_is_added_or_removed_only_if_asked() {
        for remove in [false, true] {
            assert_eq!(decide(Pair::OnlySource, remove), Step::Add);
        }
        assert_eq!(decide(Pair::OnlyDestination, true), Step::Remove);
        assert_eq!(decide(Pair::OnlyDestination, false), Step::Keep);
    }

    #[test]
    fn things_of_one_kind_are_replaced_touched_or_kept() {
        for kind in KINDS {
            for remove in [false, true] {
                let step = |content, manner| decide(both(kind, kind, content, manner), remove);
                assert_eq!(step(true, true), Step::Keep);
                assert_eq!(step(true, false), Step::Touch);
                // What it holds comes first: there is no use in setting
                // the time of a file that is about to be replaced.
                assert_eq!(step(false, true), Step::Replace);
                assert_eq!(step(false, false), Step::Replace);
            }
        }
    }

    #[test]
    fn a_thing_of_another_kind_is_replaced_unless_a_directory_would_be_lost() {
        for source in KINDS {
            for destination in KINDS {
                if source == destination {
                    continue;
                }
                for (content, manner) in [(true, true), (false, false)] {
                    let pair = both(source, destination, content, manner);
                    assert_eq!(decide(pair, true), Step::Replace);
                    let spared = if destination == Kind::Directory {
                        Step::Blocked
                    } else {
                        Step::Replace
                    };
                    assert_eq!(decide(pair, false), spared);
                }
            }
        }
    }
}
