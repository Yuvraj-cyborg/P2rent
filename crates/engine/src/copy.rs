//! Copying what a source holds into a place.
//!
//! This is the whole of a transfer from a source whose digests are not
//! known, in the order it happens: ask the source what it holds, leave
//! alone what is already there, fetch the rest, and say how far the bytes
//! were checked.
//!
//! The same files may be held by several sources, and then each chunk
//! comes from whichever of them is free to send it.

use crate::{Error, Options, Store, supplier, transfer};
use model::artifact::{Artifact, FileDigest, Tree};
use model::place::{
    Description, Event, Failure, Foreknown, Observer, Sink, Source, SupplierId, Verification,
};
use std::sync::Arc;

/// This end of a copy, as the transfer tells it from the sources.
pub(crate) const HERE: SupplierId = [0; 32];

/// The source that was given at place `number`, as the transfer tells it
/// from the others.
///
/// A transfer starts fetching at a chunk that differs for each pair of
/// ends, so that ends which trade chunks collect different ones first. A
/// copy has nobody to trade with, and its ids are chosen for it to start at
/// the first chunk.
pub(crate) fn source_id(number: usize) -> SupplierId {
    let mut id = [0; 32];
    id[..8].copy_from_slice(&(number as u64 + 1).to_le_bytes());
    id
}

/// What a copy would bring, worked out from what its sources say and
/// before anything is read or written.
#[derive(Debug)]
pub struct Plan {
    /// The tree the sources hold, under the name the copy would have.
    pub tree: Arc<Tree>,
    /// What is known beforehand of each entry.
    known: Vec<Foreknown>,
}

impl Plan {
    /// The number of files in the tree. Directories and links are not
    /// counted.
    pub fn files(&self) -> u64 {
        let files = self.tree.entries().iter().filter(|entry| entry.is_file());
        files.count() as u64
    }

    /// The digest known for each file that has one, with the file's place
    /// among the entries of the tree.
    pub fn digests(&self) -> impl Iterator<Item = (usize, FileDigest)> + '_ {
        let known = self.known.iter().enumerate();
        known.filter_map(|(file, known)| Some((file, known.digest?)))
    }

    /// How far the copy would be checked: verified if a digest is known of
    /// every file that has bytes in it.
    pub fn verification(&self) -> Verification {
        let entries = self.tree.entries().iter().zip(&self.known);
        let mut with_bytes = entries.filter(|(entry, _)| entry.size() > 0);
        if with_bytes.all(|(_, known)| known.digest.is_some()) {
            Verification::Verified
        } else {
            Verification::Consistent
        }
    }

    /// Whether every file of the tree is already in `place`, as far as
    /// size and time can tell. A tree with no file in it is not: there is
    /// nothing to go by.
    ///
    /// # Errors
    ///
    /// Returns a failure if the place cannot be looked at.
    pub async fn is_in<P: Sink + 'static>(&self, place: &Arc<P>) -> Result<bool, Failure> {
        is_current(&self.tree, place).await
    }
}

/// Works out what a copy from `sources` would bring. See [`copy`], which
/// begins with this.
///
/// # Errors
///
/// Returns a failure if a source cannot be described, if the sources do
/// not agree, or if what they describe cannot be stored.
pub async fn plan(
    sources: &[Arc<dyn Source>],
    name: Option<String>,
    chunk_size: u32,
) -> Result<Plan, Failure> {
    let described = describe(sources).await?;
    let name = name.unwrap_or(described.name);
    let tree = Tree::new(name, chunk_size, described.entries).map_err(|e| {
        Failure::Corrupt(format!(
            "the source describes a tree that cannot be stored: {e}"
        ))
    })?;
    Ok(Plan {
        tree: Arc::new(tree),
        known: described.known,
    })
}

/// What a copy did.
#[derive(Debug)]
pub struct Copied {
    /// The tree that is now in the place.
    pub tree: Arc<Tree>,
    /// The artifact that tree is. `None` if nothing was read: then no
    /// digest was learned.
    pub artifact: Option<Arc<Artifact>>,
    /// How far the bytes were checked. `None` if every file was there
    /// already and was taken on its size and time.
    pub verification: Option<Verification>,
    /// Bytes fetched from the sources.
    pub fetched: u64,
    /// Bytes kept from an earlier copy that was interrupted.
    pub resumed: u64,
    /// Bytes fetched from each source, in the order the sources were
    /// given.
    pub from: Vec<u64>,
}

/// Copies what `sources` hold into a place for it, which `open` makes from
/// the tree they describe. Files are cut into chunks of `chunk_size` to be
/// fetched. The tree is called what the first source calls it, or `name`
/// if one is given.
///
/// A file that is already there, with the size and the time the first
/// source gives, is left alone and not read. What an earlier copy left
/// unfinished is carried on from, if it was of the same version of the
/// file.
///
/// With more than one source, each must hold the same files, and a digest
/// of every file must be known: nothing else tells that the bytes of one
/// source belong with the bytes of another.
///
/// # Errors
///
/// Returns [`Error::Place`] if a source cannot be described, if the
/// sources do not agree, or if the place cannot be written,
/// [`Error::Changed`] if a source changes while it is read,
/// [`Error::Unsupplied`] if every source turns out to be of no use, and
/// [`Error::Stalled`] if nothing arrives within the stall timeout.
pub async fn copy<P, F>(
    sources: Vec<Arc<dyn Source>>,
    name: Option<String>,
    chunk_size: u32,
    open: F,
    options: Options,
    observer: Observer,
) -> Result<Copied, Error>
where
    P: Source + Sink + 'static,
    F: FnOnce(&Arc<Tree>) -> Arc<P>,
{
    let planned = plan(&sources, name, chunk_size).await?;
    let tree = planned.tree.clone();
    observer(Event::Planned {
        bytes: tree.total_size(),
        files: planned.files(),
    });
    let place = open(&tree);

    if planned.is_in(&place).await? {
        observer(Event::Kept {
            bytes: tree.total_size(),
        });
        return Ok(Copied {
            tree,
            artifact: None,
            verification: None,
            fetched: 0,
            resumed: 0,
            from: vec![0; sources.len()],
        });
    }

    let examined = {
        let observer = observer.clone();
        move |bytes| observer(Event::Examined { bytes })
    };
    let store = Store::learn(tree.clone(), place, planned.known, examined).await?;
    let held: u64 = (0..tree.chunk_count())
        .filter(|&index| store.has(index))
        .filter_map(|index| tree.locate(index))
        .map(|chunk| u64::from(chunk.len))
        .sum();
    observer(Event::Kept { bytes: held });
    let count = sources.len();
    let suppliers = sources
        .into_iter()
        .enumerate()
        .map(|(number, source)| supplier(source_id(number), tree.clone(), source))
        .collect();
    let summary = transfer(&store, HERE, suppliers, options, observer).await?;
    store.finish().await?;
    let stored_from = |number| {
        let source = summary
            .sources
            .iter()
            .find(|(id, _)| *id == source_id(number));
        source.map_or(0, |(_, bytes)| *bytes)
    };
    Ok(Copied {
        artifact: store.artifact(),
        verification: Some(summary.verification),
        fetched: tree.total_size() - held,
        resumed: held,
        from: (0..count).map(stored_from).collect(),
        tree,
    })
}

/// What the sources hold, which has to be the same for each of them.
///
/// Read from one source, a file is of one version for as long as the
/// source says so, and what the source gives to tell versions apart is
/// what a transfer notes beside the file. Read from several, only a digest
/// of the whole file tells that their bytes belong together. So every file
/// must have one, and the digest is what is noted.
async fn describe(sources: &[Arc<dyn Source>]) -> Result<Description, Failure> {
    let mut described = Vec::with_capacity(sources.len());
    for source in sources {
        described.push(source.describe().await?);
    }
    let mut others = described.into_iter();
    let Some(mut first) = others.next() else {
        return Err(Failure::Fatal("there is no source to copy from".into()));
    };
    for (other, number) in others.zip(2..) {
        let same_entries = other.entries.len() == first.entries.len()
            && (other.entries.iter().zip(&first.entries)).all(|(theirs, ours)| theirs.0 == ours.0);
        if !same_entries {
            return Err(Failure::Fatal(format!(
                "source {number} does not hold the same files as the first"
            )));
        }
        for (known, theirs) in first.known.iter_mut().zip(other.known) {
            if known.digest.is_some() && theirs.digest.is_some() && known.digest != theirs.digest {
                return Err(Failure::Fatal(format!(
                    "source {number} gives another digest for a file than the first"
                )));
            }
            known.digest = known.digest.or(theirs.digest);
        }
    }
    if sources.len() > 1 {
        for ((entry, _), known) in first.entries.iter().zip(&mut first.known) {
            match known.digest {
                Some(digest) => known.validator = digest.to_string().into_bytes(),
                None if entry.size() > 0 => {
                    let name = [first.name.as_str(), entry.path.as_str()];
                    let name = name.iter().filter(|part| !part.is_empty());
                    return Err(Failure::Fatal(format!(
                        "{} cannot be read from more than one source: without a digest of \
                         the file, nothing tells that they hold the same one",
                        name.copied().collect::<Vec<_>>().join("/")
                    )));
                }
                None => {}
            }
        }
    }
    Ok(first)
}

/// See [`Plan::is_in`].
async fn is_current<P: Sink + 'static>(tree: &Arc<Tree>, place: &Arc<P>) -> Result<bool, Failure> {
    let (tree, place) = (tree.clone(), place.clone());
    let looked = tokio::task::spawn_blocking(move || {
        let mut files = tree
            .entries()
            .iter()
            .enumerate()
            .filter(|(_, e)| e.is_file());
        let mut any = false;
        files
            .try_fold(true, |all, (file, _)| {
                any = true;
                Ok(all && place.is_current(file)?)
            })
            .map(|all| all && any)
    });
    looked.await.unwrap_or_else(|stopped| {
        Err(Failure::Fatal(format!(
            "looking at what is already there was cut short: {stopped}"
        )))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conformance::{Memory, Sample};
    use model::artifact::{Hash, MIN_CHUNK_SIZE, Timestamp};
    use model::place::{Arrived, Capabilities, Pending, Reader};
    use std::sync::Mutex;

    /// A place that is read from, and says of its files what it is told to.
    #[derive(Debug)]
    struct Told {
        place: Memory,
        known: Vec<Foreknown>,
        /// The time it gives for every file, if any.
        modified: Option<Timestamp>,
        /// How many bytes of each file can be read before the line goes
        /// dead.
        reach: u64,
    }

    /// A reader that can be read so far and no further.
    #[derive(Debug)]
    struct Short {
        reader: Box<dyn Reader>,
        left: u64,
    }

    impl Reader for Short {
        fn read<'a>(&'a mut self, len: u32, arrived: &'a Arrived) -> Pending<'a, Vec<u8>> {
            Box::pin(async move {
                let left = self.left.checked_sub(u64::from(len));
                self.left =
                    left.ok_or_else(|| Failure::Unreachable("the line went dead".into()))?;
                self.reader.read(len, arrived).await
            })
        }
    }

    impl Source for Told {
        fn capabilities(&self) -> Capabilities {
            Source::capabilities(&self.place)
        }

        fn describe(&self) -> Pending<'_, Description> {
            Box::pin(async move {
                let mut described = self.place.describe().await?;
                described.known.clone_from(&self.known);
                for (_, recorded) in &mut described.entries {
                    recorded.modified = self.modified;
                }
                Ok(described)
            })
        }

        fn open(&self, file: usize, offset: u64) -> Pending<'_, Box<dyn Reader>> {
            Box::pin(async move {
                let short: Box<dyn Reader> = Box::new(Short {
                    reader: self.place.open(file, offset).await?,
                    left: self.reach.saturating_sub(offset),
                });
                Ok(short)
            })
        }
    }

    /// What is known of each file of the sample: a validator, and the
    /// digest of the file if `digests` are given.
    fn known(sample: &Sample, digests: bool) -> Vec<Foreknown> {
        let of = |bytes: &Vec<u8>| Foreknown {
            validator: b"v1".to_vec(),
            digest: (digests && !bytes.is_empty()).then(|| FileDigest::Blake3(Hash::of(bytes))),
        };
        sample.files.iter().map(of).collect()
    }

    /// A source that holds `files` and says `known` of them.
    fn source(sample: &Sample, files: &[Vec<u8>], known: Vec<Foreknown>) -> Told {
        Told {
            place: Memory::holding(sample.artifact.tree().clone(), files).unwrap(),
            known,
            modified: None,
            reach: u64::MAX,
        }
    }

    /// Copies from `sources` into `place`, or into a new place if there is
    /// none, which is returned with what the copy did.
    fn copy_into(
        place: Option<Memory>,
        sources: Vec<Told>,
        options: Options,
    ) -> (Result<Copied, Error>, Memory) {
        let (copied, place, _) = copy_as(place, sources, None, options);
        (copied, place.unwrap())
    }

    /// The same, under the name `name` if one is given. Also returns what
    /// the copy reported as it went.
    fn copy_as(
        place: Option<Memory>,
        sources: Vec<Told>,
        name: Option<&str>,
        options: Options,
    ) -> (Result<Copied, Error>, Option<Memory>, Vec<Event>) {
        let events: Arc<Mutex<Vec<Event>>> = Arc::default();
        let seen: Observer = {
            let events = events.clone();
            Arc::new(move |event| events.lock().unwrap().push(event))
        };
        let sources = sources
            .into_iter()
            .map(|source| Arc::new(source) as Arc<dyn Source>)
            .collect();
        let made: Arc<Mutex<Option<Memory>>> = Arc::new(Mutex::new(place));
        let open = {
            let made = made.clone();
            move |tree: &Arc<Tree>| {
                let mut made = made.lock().unwrap();
                Arc::new(
                    made.get_or_insert_with(|| Memory::new(tree.clone()))
                        .clone(),
                )
            }
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .start_paused(true)
            .build()
            .unwrap();
        let copied = runtime.block_on(copy(
            sources,
            name.map(str::to_owned),
            MIN_CHUNK_SIZE,
            open,
            options,
            seen,
        ));
        let place = made.lock().unwrap().clone();
        let events = events.lock().unwrap().clone();
        (copied, place, events)
    }

    fn holds_the_sample(place: &Memory, sample: &Sample) -> bool {
        let files = sample.artifact.entries().iter().enumerate();
        files
            .filter(|(_, entry)| entry.is_file())
            .all(|(file, _)| place.committed(file).as_ref() == Some(&sample.files[file]))
    }

    #[test]
    fn a_copy_fetches_what_its_source_holds() {
        let sample = Sample::new().unwrap();
        let total = sample.artifact.total_size();
        let from = source(&sample, &sample.files, known(&sample, false));
        let (copied, place) = copy_into(None, vec![from], Options::default());
        let copied = copied.unwrap();

        assert!(holds_the_sample(&place, &sample));
        assert!(place.is_finished());
        assert_eq!((copied.fetched, copied.resumed), (total, 0));
        assert_eq!(copied.from, [total]);
        assert_eq!(copied.verification, Some(Verification::Consistent));
        assert_eq!(copied.artifact.as_deref(), Some(&*sample.artifact));
        assert_eq!(copied.tree.name(), "sample");
    }

    /// A copy says what there is to copy before it moves anything, and
    /// can be given another name than the source has for it.
    #[test]
    fn a_copy_says_what_there_is_and_takes_the_name_it_is_given() {
        let sample = Sample::new().unwrap();
        let total = sample.artifact.total_size();
        let from = || vec![source(&sample, &sample.files, known(&sample, false))];

        let (copied, _, events) = copy_as(None, from(), None, Options::default());
        assert_eq!(copied.unwrap().tree.name(), "sample");
        // Four files: the directories are not counted.
        let planned = Event::Planned {
            bytes: total,
            files: 4,
        };
        assert_eq!(events.first(), Some(&planned));
        // Nothing was there, so nothing was kept.
        assert!(events.contains(&Event::Kept { bytes: 0 }), "{events:?}");
        let stored = events.iter().filter_map(|event| match event {
            Event::Stored { bytes, .. } => Some(bytes),
            _ => None,
        });
        assert_eq!(stored.sum::<u64>(), total);

        let (copied, place, _) = copy_as(None, from(), Some("renamed"), Options::default());
        assert_eq!(copied.unwrap().tree.name(), "renamed");
        assert!(holds_the_sample(&place.unwrap(), &sample));

        // A name that no file can have is refused before anything is read,
        // and before there is a place to put it.
        let (copied, place, events) = copy_as(None, from(), Some("a/b"), Options::default());
        assert!(
            matches!(copied, Err(Error::Place(Failure::Corrupt(_)))),
            "{copied:?}"
        );
        assert!(place.is_none());
        assert_eq!(events, []);
    }

    /// A plan says what a copy would bring, and reads and writes nothing.
    #[test]
    fn a_plan_says_what_a_copy_would_bring() {
        let sample = Sample::new().unwrap();
        let from = |digests| {
            let told = source(&sample, &sample.files, known(&sample, digests));
            vec![Arc::new(told) as Arc<dyn Source>]
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();

        let sources = from(false);
        let plain = plan(&sources, Some("other".into()), MIN_CHUNK_SIZE);
        let plain = runtime.block_on(plain).unwrap();
        assert_eq!(plain.tree.name(), "other");
        assert_eq!(plain.tree.total_size(), sample.artifact.total_size());
        assert_eq!(plain.files(), 4);
        assert_eq!(plain.verification(), Verification::Consistent);
        assert_eq!(plain.digests().count(), 0);
        // Nothing is in a place that was only just made.
        let place = Arc::new(Memory::new(plain.tree.clone()));
        assert!(!runtime.block_on(plain.is_in(&place)).unwrap());

        // With a digest of every file that has bytes, it would be verified.
        let sources = from(true);
        let checked = plan(&sources, None, MIN_CHUNK_SIZE);
        let checked = runtime.block_on(checked).unwrap();
        assert_eq!(checked.verification(), Verification::Verified);
        let files: Vec<usize> = checked.digests().map(|(file, _)| file).collect();
        assert_eq!(files, [1, 4, 5]);
        // And with one of them missing, it would not.
        let mut some = known(&sample, true);
        some[4].digest = None;
        let some = vec![Arc::new(source(&sample, &sample.files, some)) as Arc<dyn Source>];
        let partly = runtime.block_on(plan(&some, None, MIN_CHUNK_SIZE)).unwrap();
        assert_eq!(partly.verification(), Verification::Consistent);
    }

    /// A file is taken for the one the source holds on its size and time,
    /// so a source that gives no time has its files fetched again.
    #[test]
    fn files_that_are_there_with_the_size_and_time_are_left_alone() {
        let sample = Sample::new().unwrap();
        let total = sample.artifact.total_size();
        for (modified, fetched_again) in [
            (
                Some(Timestamp {
                    seconds: 7,
                    nanos: 0,
                }),
                0,
            ),
            (None, total),
        ] {
            let timed = || Told {
                modified,
                ..source(&sample, &sample.files, known(&sample, false))
            };
            let (first, place) = copy_into(None, vec![timed()], Options::default());
            assert_eq!(first.unwrap().fetched, total);

            let (again, place) = copy_into(Some(place), vec![timed()], Options::default());
            let again = again.unwrap();
            assert_eq!(again.fetched, fetched_again);
            assert_eq!(again.from, [fetched_again]);
            assert_eq!(again.verification.is_none(), fetched_again == 0);
            assert_eq!(again.artifact.is_none(), fetched_again == 0);
            assert!(holds_the_sample(&place, &sample));
        }
    }

    /// What an interrupted copy stored of a file is kept by the next one,
    /// which fetches the rest.
    #[test]
    fn an_interrupted_copy_is_carried_on_with() {
        let sample = Sample::new().unwrap();
        let total = sample.artifact.total_size();
        let two_chunks = u64::from(MIN_CHUNK_SIZE) * 2;
        let short = Told {
            reach: two_chunks,
            ..source(&sample, &sample.files, known(&sample, false))
        };
        // Through one pipeline, so that it is the third chunk of the first
        // file that cannot be had, every time it is tried.
        let one_at_a_time = Options {
            requests_per_supplier: 1,
            ..Options::default()
        };
        let (first, place) = copy_into(None, vec![short], one_at_a_time);
        assert!(matches!(first, Err(Error::Stalled(_))), "{first:?}");

        let whole = source(&sample, &sample.files, known(&sample, false));
        let (second, place) = copy_into(Some(place), vec![whole], Options::default());
        let second = second.unwrap();
        assert!(holds_the_sample(&place, &sample));
        // The two chunks of the first file that were noted are kept.
        assert_eq!(
            (second.fetched, second.resumed),
            (total - two_chunks, two_chunks)
        );
        assert_eq!(second.from, [total - two_chunks]);
    }

    /// What a copy keeps of what was there is reported before it fetches:
    /// the chunks an interrupted copy noted, and everything, if every file
    /// is there already.
    #[test]
    fn a_copy_says_how_much_it_keeps() {
        let sample = Sample::new().unwrap();
        let total = sample.artifact.total_size();
        let two_chunks = u64::from(MIN_CHUNK_SIZE) * 2;
        let short = Told {
            reach: two_chunks,
            ..source(&sample, &sample.files, known(&sample, false))
        };
        let one_at_a_time = Options {
            requests_per_supplier: 1,
            ..Options::default()
        };
        let (_, place, _) = copy_as(None, vec![short], None, one_at_a_time);
        let whole = source(&sample, &sample.files, known(&sample, false));
        let (_, _, events) = copy_as(place, vec![whole], None, Options::default());
        let kept = events
            .iter()
            .position(|event| *event == Event::Kept { bytes: two_chunks });
        let stored = events
            .iter()
            .position(|event| matches!(event, Event::Stored { .. }));
        assert!(kept.is_some() && kept < stored, "{events:?}");

        let timed = || Told {
            modified: Some(Timestamp {
                seconds: 7,
                nanos: 0,
            }),
            ..source(&sample, &sample.files, known(&sample, false))
        };
        let (_, place, _) = copy_as(None, vec![timed()], None, Options::default());
        let (_, _, events) = copy_as(place, vec![timed()], None, Options::default());
        let planned = Event::Planned {
            bytes: total,
            files: 4,
        };
        assert_eq!(events, [planned, Event::Kept { bytes: total }]);
    }

    #[test]
    fn several_sources_that_hold_the_same_files_feed_one_copy() {
        let sample = Sample::new().unwrap();
        let total = sample.artifact.total_size();
        let one = || source(&sample, &sample.files, known(&sample, true));
        let (copied, place) = copy_into(None, vec![one(), one(), one()], Options::default());
        let copied = copied.unwrap();

        assert!(holds_the_sample(&place, &sample));
        assert_eq!(copied.verification, Some(Verification::Verified));
        assert_eq!(copied.from.len(), 3);
        assert_eq!(copied.from.iter().sum::<u64>(), total);

        // One that cannot be read is made up for by another, and one that
        // gives no digest is believed on the digest of another.
        let gone = Told { reach: 0, ..one() };
        let silent = source(&sample, &sample.files, known(&sample, false));
        let (copied, place) = copy_into(None, vec![gone, silent, one()], Options::default());
        let copied = copied.unwrap();
        assert!(holds_the_sample(&place, &sample));
        assert_eq!(copied.verification, Some(Verification::Verified));
        assert_eq!(copied.from[0], 0);
        assert_eq!(copied.from[1] + copied.from[2], total);
    }

    #[test]
    fn sources_that_cannot_be_known_to_hold_the_same_files_are_not_read() {
        let sample = Sample::new().unwrap();
        let one = || source(&sample, &sample.files, known(&sample, true));
        let silent = || source(&sample, &sample.files, known(&sample, false));
        let mut other_digest = known(&sample, true);
        other_digest[1].digest = Some(FileDigest::Blake3(Hash::of(b"another file")));
        let disagreeing = source(&sample, &sample.files, other_digest);
        // A tree with one file fewer.
        let tree = sample.artifact.tree();
        let fewer = (0..5)
            .filter_map(|file| Some((tree.entries().get(file)?.clone(), tree.metadata(file)?)));
        let fewer = Tree::new("sample", MIN_CHUNK_SIZE, fewer.collect()).unwrap();
        let smaller = Told {
            place: Memory::new(Arc::new(fewer)),
            ..one()
        };

        for (sources, why) in [
            (vec![], "there is no source"),
            (
                vec![silent(), silent()],
                "sample/a.bin cannot be read from more than one",
            ),
            (vec![one(), disagreeing], "source 2 gives another digest"),
            (
                vec![one(), one(), smaller],
                "source 3 does not hold the same files",
            ),
        ] {
            let empty = Memory::new(sample.artifact.tree().clone());
            let (copied, _) = copy_into(Some(empty), sources, Options::default());
            let Err(Error::Place(Failure::Fatal(refused))) = &copied else {
                panic!("{copied:?}");
            };
            assert!(refused.contains(why), "{refused}");
        }
    }

    /// The digest is what tells that a source holds other bytes: nothing
    /// of the file is kept.
    #[test]
    fn a_source_that_holds_other_bytes_is_found_out_by_the_digest() {
        let sample = Sample::new().unwrap();
        let mut other_bytes = sample.files.clone();
        other_bytes[1][0] ^= 0xff;
        let honest_and_gone = Told {
            reach: 0,
            ..source(&sample, &sample.files, known(&sample, true))
        };
        let wrong = source(&sample, &other_bytes, known(&sample, false));
        let (copied, place) = copy_into(None, vec![honest_and_gone, wrong], Options::default());

        let Err(Error::Place(Failure::Corrupt(why))) = &copied else {
            panic!("{copied:?}");
        };
        assert!(why.contains("a.bin"), "{why}");
        assert_eq!(place.committed(1), None);
    }
}
