//! Transfers from suppliers that misbehave.
//!
//! The suppliers here are made up: each follows a script that says what it
//! holds, when it lies, when it breaks off and how slow it is. Whatever the
//! scripts, a transfer must end with the right bytes or with an error, and
//! never with a wrong file. Time is the test's own, so a minute of waiting
//! takes no time at all.

use crate::conformance::{Memory, Sample};
use crate::{Error, Options, Store, Summary, transfer};
use model::artifact::{Bitfield, FileDigest, Hash};
use model::place::{
    Capabilities, Description, Event, Expected, Failure, Foreknown, Observer, Pending, Reader,
    Sink, Source, Status, Supplier, SupplierId, Supply, Verification,
};
use proptest::collection::vec;
use proptest::prelude::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// This end of every transfer here.
const ME: SupplierId = [0xee; 32];

/// How a made-up supplier behaves, chunk by chunk.
#[derive(Debug, Clone, Default)]
struct Script {
    /// The chunks it says it holds.
    holds: Vec<bool>,
    /// The chunks it sends wrong bytes for.
    lies: Vec<bool>,
    /// The chunks it says it holds and then does not have.
    withholds: Vec<bool>,
    /// It breaks off after this many chunks, each time it is reached.
    breaks_after: Option<usize>,
    /// How long it takes over each chunk.
    delay: Duration,
    /// How many times it cannot be reached before it can.
    unreachable_for: usize,
    /// After this many chunks, what it reads from has changed.
    changes_after: Option<usize>,
    /// It is one stream: it gives its chunks only in order, one at a time.
    in_order: bool,
    /// Suppliers it tells of.
    others: Vec<Made>,
}

impl Script {
    /// A supplier that holds everything and does nothing wrong.
    fn honest(sample: &Sample) -> Self {
        Self {
            holds: vec![true; chunks(sample)],
            ..Self::default()
        }
    }

    fn says(list: &[bool], index: u64) -> bool {
        usize::try_from(index)
            .ok()
            .and_then(|index| list.get(index))
            .is_some_and(|&yes| yes)
    }

    /// Whether it supplies every chunk, truthfully, for as long as asked.
    fn is_reliable(&self, chunks: u64) -> bool {
        let every_chunk = (0..chunks).all(|index| {
            Self::says(&self.holds, index)
                && !Self::says(&self.lies, index)
                && !Self::says(&self.withholds, index)
        });
        every_chunk && self.breaks_after.is_none()
    }
}

/// A supplier that follows a script.
#[derive(Debug, Clone)]
struct Made(Arc<Inner>);

#[derive(Debug)]
struct Inner {
    id: SupplierId,
    script: Script,
    sample: Arc<Sample>,
    /// How many times it has been reached for.
    opened: AtomicUsize,
    handed_over: Hook,
    /// Every chunk it has been asked for, in the order it was asked.
    asked: Mutex<Vec<u64>>,
}

/// Called each time a supplier hands over a chunk that is right.
#[derive(Clone)]
struct Hook(Arc<dyn Fn() + Send + Sync>);

impl std::fmt::Debug for Hook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Hook")
    }
}

impl Made {
    fn new(number: u8, script: Script, sample: &Arc<Sample>) -> Self {
        Self::watched(number, script, sample, Hook(Arc::new(|| {})))
    }

    fn watched(number: u8, script: Script, sample: &Arc<Sample>, handed_over: Hook) -> Self {
        Self(Arc::new(Inner {
            id: [number; 32],
            script,
            sample: sample.clone(),
            opened: AtomicUsize::new(0),
            handed_over,
            asked: Mutex::default(),
        }))
    }
}

impl Supplier for Made {
    fn id(&self) -> SupplierId {
        self.0.id
    }

    fn open(&self) -> Pending<'_, Arc<dyn Supply>> {
        Box::pin(async move {
            let attempt = self.0.opened.fetch_add(1, Ordering::Relaxed);
            if attempt < self.0.script.unreachable_for {
                return Err(Failure::Unreachable("nobody answers".into()));
            }
            let reached: Arc<dyn Supply> = Arc::new(Reached {
                made: self.0.clone(),
                sent: AtomicUsize::new(0),
            });
            Ok(reached)
        })
    }
}

/// One time a made-up supplier was reached.
#[derive(Debug)]
struct Reached {
    made: Arc<Inner>,
    /// Chunks sent since it was reached.
    sent: AtomicUsize,
}

impl Supply for Reached {
    fn status(&self) -> Pending<'_, Status> {
        Box::pin(async move {
            let script = &self.made.script;
            let chunks = self.made.sample.artifact.chunk_count();
            let mut held = Bitfield::new(chunks);
            for index in (0..chunks).filter(|&index| Script::says(&script.holds, index)) {
                held.set(index);
            }
            let others = script
                .others
                .iter()
                .map(|other| Arc::new(other.clone()) as Arc<dyn Supplier>)
                .collect();
            Ok(Status { held, others })
        })
    }

    fn chunk(&self, index: u64) -> Pending<'_, Vec<u8>> {
        Box::pin(async move {
            let script = &self.made.script;
            self.made.asked.lock().unwrap().push(index);
            tokio::time::sleep(script.delay).await;
            let sent = self.sent.fetch_add(1, Ordering::Relaxed);
            if script.breaks_after.is_some_and(|limit| sent >= limit) {
                return Err(Failure::Unreachable("the line went dead".into()));
            }
            if script.changes_after.is_some_and(|limit| sent >= limit) {
                return Err(Failure::Changed("the file is not what it was".into()));
            }
            if Script::says(&script.withholds, index) {
                return Err(Failure::Absent(format!("no chunk {index} after all")));
            }
            let mut data = self.made.sample.chunk(index);
            if Script::says(&script.lies, index) {
                data[0] ^= 0xff;
            } else {
                (self.made.handed_over.0)();
            }
            Ok(data)
        })
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            random_access: !self.made.script.in_order,
        }
    }
}

/// A place in memory with something done before each write.
struct Hooked {
    place: Memory,
    before_write: Box<dyn Fn() -> Result<(), Failure> + Send + Sync>,
}

impl std::fmt::Debug for Hooked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.place.fmt(f)
    }
}

impl Source for Hooked {
    fn capabilities(&self) -> Capabilities {
        Source::capabilities(&self.place)
    }

    fn describe(&self) -> Pending<'_, Description> {
        self.place.describe()
    }

    fn open(&self, file: usize, offset: u64) -> Pending<'_, Box<dyn Reader>> {
        self.place.open(file, offset)
    }
}

impl Sink for Hooked {
    fn capabilities(&self) -> Capabilities {
        Sink::capabilities(&self.place)
    }

    fn begin(
        &self,
        expected: &Expected<'_>,
        examined: &(dyn Fn(u64) + Sync),
    ) -> Result<Bitfield, Failure> {
        self.place.begin(expected, examined)
    }

    fn write(&self, file: usize, offset: u64, data: &[u8]) -> Result<(), Failure> {
        (self.before_write)()?;
        self.place.write(file, offset, data)
    }

    fn read_back(&self, file: usize, offset: u64, len: u32) -> Result<Vec<u8>, Failure> {
        self.place.read_back(file, offset, len)
    }

    fn notes(&self, file: usize, limit: usize) -> Result<Vec<u8>, Failure> {
        self.place.notes(file, limit)
    }

    fn note(&self, file: usize, offset: u64, data: &[u8]) -> Result<(), Failure> {
        self.place.note(file, offset, data)
    }

    fn commit(&self, file: usize) -> Result<(), Failure> {
        self.place.commit(file)
    }

    fn abort(&self, file: usize) -> Result<(), Failure> {
        self.place.abort(file)
    }

    fn is_current(&self, file: usize) -> Result<bool, Failure> {
        self.place.is_current(file)
    }

    fn finish(&self) -> Result<(), Failure> {
        self.place.finish()
    }
}

/// Everything one transfer left behind.
struct Ran {
    outcome: Result<Summary, Error>,
    place: Memory,
    store: Arc<Store>,
    events: Vec<Event>,
    /// How long the transfer took, by the test's clock.
    took: Duration,
}

/// One transfer of the sample into a place in memory.
struct Setup {
    sample: Arc<Sample>,
    options: Options,
    before_write: Box<dyn Fn() -> Result<(), Failure> + Send + Sync>,
    on_event: Box<dyn Fn(Event) + Send + Sync>,
    /// The place to fill. A new, empty one if `None`.
    place: Option<Memory>,
    /// If set, the digests of the chunks are not known beforehand, and this
    /// is what is known of each entry instead.
    foreknown: Option<Vec<Foreknown>>,
}

impl Setup {
    fn new(sample: &Arc<Sample>) -> Self {
        Self {
            sample: sample.clone(),
            options: Options {
                stall_timeout: Duration::from_secs(90),
                ..Options::default()
            },
            before_write: Box::new(|| Ok(())),
            on_event: Box::new(|_| {}),
            place: None,
            foreknown: None,
        }
    }

    /// A transfer that knows no chunk's digest, and of each file knows
    /// `validator` and no digest.
    fn learning(sample: &Arc<Sample>, validator: &[u8]) -> Self {
        let known = Foreknown {
            validator: validator.to_vec(),
            digest: None,
        };
        Self {
            foreknown: Some(vec![known; sample.artifact.entries().len()]),
            ..Self::new(sample)
        }
    }

    /// The same transfer, into a place an earlier one left.
    fn into(mut self, place: &Memory) -> Self {
        self.place = Some(place.clone());
        self
    }

    /// Runs the transfer, with a clock that jumps to the next thing due.
    fn run(self, suppliers: &[Made]) -> Ran {
        let suppliers = suppliers
            .iter()
            .map(|made| Arc::new(made.clone()) as Arc<dyn Supplier>)
            .collect();
        self.run_from(suppliers)
    }

    /// The same, from suppliers of any kind.
    fn run_from(self, suppliers: Vec<Arc<dyn Supplier>>) -> Ran {
        let tree = self.sample.artifact.tree().clone();
        let place = self.place.unwrap_or_else(|| Memory::new(tree.clone()));
        let hooked = Arc::new(Hooked {
            place: place.clone(),
            before_write: self.before_write,
        });
        let events: Arc<Mutex<Vec<Event>>> = Arc::default();
        let (seen, on_event) = (events.clone(), self.on_event);
        let observer: Observer = Arc::new(move |event| {
            on_event(event);
            seen.lock().unwrap().push(event);
        });

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .start_paused(true)
            .build()
            .unwrap();
        let (outcome, store, took) = runtime.block_on(async {
            let store = match self.foreknown {
                Some(files) => Store::learn(tree, hooked, files, |_| {}).await,
                None => Store::resume(self.sample.artifact.clone(), hooked, |_| {}).await,
            }
            .unwrap();
            let started = tokio::time::Instant::now();
            // A transfer always ends, with every chunk or by stalling. One
            // that does not is a fault, and is not waited for.
            let fetching = transfer(&store, ME, suppliers, self.options, observer);
            let outcome = tokio::time::timeout(Duration::from_secs(3600), fetching)
                .await
                .expect("the transfer never ended");
            (outcome, store, started.elapsed())
        });
        let events = events.lock().unwrap().clone();
        Ran {
            outcome,
            place,
            store,
            events,
            took,
        }
    }
}

impl Ran {
    /// No file has its final name unless it is the file that was sent.
    fn has_no_wrong_file(&self, sample: &Sample) -> bool {
        sample.files.iter().enumerate().all(|(file, bytes)| {
            self.place
                .committed(file)
                .is_none_or(|committed| &committed == bytes)
        })
    }

    /// Every file has its final name and the bytes that were sent.
    fn has_every_file(&self, sample: &Sample) -> bool {
        let files = sample.artifact.entries().iter().enumerate();
        files
            .filter(|(_, entry)| entry.is_file())
            .all(|(file, _)| self.place.committed(file).as_ref() == Some(&sample.files[file]))
    }

    fn stored_from(&self, number: u8) -> u64 {
        let Ok(summary) = &self.outcome else { return 0 };
        let from = summary.sources.iter().find(|(id, _)| *id == [number; 32]);
        from.map_or(0, |(_, bytes)| *bytes)
    }

    fn count(&self, wanted: fn(&Event) -> bool) -> usize {
        self.events.iter().filter(|event| wanted(event)).count()
    }
}

fn sample() -> Arc<Sample> {
    Arc::new(Sample::new().unwrap())
}

fn chunks(sample: &Sample) -> usize {
    usize::try_from(sample.artifact.chunk_count()).unwrap()
}

#[test]
fn one_honest_supplier_is_enough() {
    let sample = sample();
    let supplier = Made::new(1, Script::honest(&sample), &sample);
    let ran = Setup::new(&sample).run(&[supplier]);

    let summary = ran.outcome.as_ref().unwrap();
    assert!(ran.store.is_complete());
    assert!(ran.has_every_file(&sample));
    assert_eq!(summary.sources, [([1; 32], sample.artifact.total_size())]);
    assert_eq!(summary.usable, [[1; 32]]);
    assert_eq!(summary.verification, Verification::Verified);
}

#[test]
fn everything_that_happens_is_reported_once() {
    let sample = sample();
    let supplier = Made::new(1, Script::honest(&sample), &sample);
    let ran = Setup::new(&sample).run(&[supplier]);

    assert_eq!(ran.count(|e| matches!(e, Event::Opened { .. })), 1);
    assert_eq!(ran.count(|e| matches!(e, Event::Closed { .. })), 1);
    assert_eq!(
        ran.count(|e| matches!(e, Event::Stored { .. })),
        chunks(&sample)
    );
    let stored: u64 = ran
        .events
        .iter()
        .map(|event| match event {
            Event::Stored { bytes, supplier } if *supplier == [1; 32] => *bytes,
            _ => 0,
        })
        .sum();
    assert_eq!(stored, sample.artifact.total_size());
    // One for each file that has bytes. The empty file was there already.
    let mut committed: Vec<usize> = ran
        .events
        .iter()
        .filter_map(|event| match event {
            Event::Committed { file } => Some(*file),
            _ => None,
        })
        .collect();
    committed.sort_unstable();
    assert_eq!(committed, [1, 4, 5]);
}

#[test]
fn a_supplier_that_lies_is_dropped_and_an_honest_one_finishes() {
    let sample = sample();
    let liar = Script {
        lies: vec![true; chunks(&sample)],
        ..Script::honest(&sample)
    };
    // The honest one takes its time, by the test's clock, so the liar is
    // always caught first. Were both to answer at once, the honest one
    // could deliver everything before a single lie had been checked, and
    // the transfer would rightly end with the liar never found out.
    let unhurried = Script {
        delay: Duration::from_secs(5),
        ..Script::honest(&sample)
    };
    let suppliers = [
        Made::new(1, liar, &sample),
        Made::new(2, unhurried, &sample),
    ];
    let ran = Setup::new(&sample).run(&suppliers);

    let summary = ran.outcome.as_ref().unwrap();
    assert!(ran.has_every_file(&sample));
    assert_eq!(ran.stored_from(1), 0, "bytes were kept from a liar");
    assert_eq!(ran.stored_from(2), sample.artifact.total_size());
    assert_eq!(summary.usable, [[2; 32]], "a liar is still counted usable");
}

/// A liar that is never caught does no harm either: with an honest supplier
/// answering at the same moment, whichever is quicker, nothing wrong is
/// kept.
#[test]
fn nothing_is_kept_from_a_liar_whoever_is_quicker() {
    let sample = sample();
    let liar = Script {
        lies: vec![true; chunks(&sample)],
        ..Script::honest(&sample)
    };
    let suppliers = [
        Made::new(1, liar, &sample),
        Made::new(2, Script::honest(&sample), &sample),
    ];
    let ran = Setup::new(&sample).run(&suppliers);

    assert!(ran.has_every_file(&sample), "{:?}", ran.outcome);
    assert_eq!(ran.stored_from(1), 0, "bytes were kept from a liar");
    assert_eq!(ran.stored_from(2), sample.artifact.total_size());
}

#[test]
fn a_supplier_that_breaks_off_is_tried_again() {
    let sample = sample();
    let flaky = Script {
        breaks_after: Some(2),
        unreachable_for: 2,
        ..Script::honest(&sample)
    };
    let ran = Setup::new(&sample).run(&[Made::new(1, flaky, &sample)]);

    assert!(ran.outcome.is_ok(), "{:?}", ran.outcome);
    assert!(ran.has_every_file(&sample));
    assert!(ran.count(|e| matches!(e, Event::Opened { .. })) > 1);
}

#[test]
fn a_chunk_a_supplier_turns_out_not_to_have_comes_from_another() {
    let sample = sample();
    let empty_handed = Script {
        withholds: vec![true; chunks(&sample)],
        ..Script::honest(&sample)
    };
    let suppliers = [
        Made::new(1, empty_handed, &sample),
        Made::new(2, Script::honest(&sample), &sample),
    ];
    let ran = Setup::new(&sample).run(&suppliers);

    assert!(ran.has_every_file(&sample));
    assert_eq!(ran.stored_from(1), 0);
    // It did nothing wrong, so it is not dropped.
    assert_eq!(ran.outcome.unwrap().usable.len(), 2);
}

#[test]
fn a_supplier_told_of_by_another_is_used() {
    let sample = sample();
    let hidden = Made::new(2, Script::honest(&sample), &sample);
    // It also names this end, which must not fetch from itself.
    let myself = Made::new(0xee, Script::honest(&sample), &sample);
    let knows_others = Script {
        others: vec![hidden, myself],
        ..Script::default()
    };
    let ran = Setup::new(&sample).run(&[Made::new(1, knows_others, &sample)]);

    assert!(ran.has_every_file(&sample));
    assert_eq!(ran.stored_from(2), sample.artifact.total_size());
    assert_eq!(ran.stored_from(0xee), 0);
    assert_eq!(ran.outcome.unwrap().usable.len(), 2);
}

#[test]
fn with_nobody_holding_a_chunk_the_transfer_stalls_and_says_so() {
    let sample = sample();
    let mut all_but_one = Script::honest(&sample);
    all_but_one.holds[0] = false;
    let mut setup = Setup::new(&sample);
    setup.options.max_suppliers = 1;
    let ran = setup.run(&[Made::new(1, all_but_one, &sample)]);

    let stalled = matches!(ran.outcome, Err(Error::Stalled(_)));
    assert!(stalled, "{:?}", ran.outcome);
    assert!(!ran.store.is_complete());
    assert!(ran.has_no_wrong_file(&sample));
    // Nobody was waiting for its turn, so it was left open, not dropped
    // and reached for again.
    assert_eq!(ran.count(|e| matches!(e, Event::Opened { .. })), 1);
    // What did arrive is kept, for the next attempt.
    assert_eq!(
        ran.store.held().count_ones(),
        sample.artifact.chunk_count() - 1
    );
}

#[test]
fn a_place_that_cannot_be_written_ends_the_transfer() {
    let sample = sample();
    let mut setup = Setup::new(&sample);
    setup.before_write = Box::new(|| Err(Failure::Fatal("no space left".into())));
    let ran = setup.run(&[Made::new(1, Script::honest(&sample), &sample)]);

    let Err(Error::Place(Failure::Fatal(reason))) = &ran.outcome else {
        panic!("{:?}", ran.outcome);
    };
    assert_eq!(reason, "no space left");
    assert!(ran.has_no_wrong_file(&sample));
}

/// A supplier that delivers and then breaks off has failed once, however
/// often it has done so before, so it is tried again at once and not after
/// a wait that grows each time.
#[test]
fn a_supplier_that_keeps_delivering_is_not_made_to_wait_longer_each_time() {
    let sample = sample();
    let one_at_a_time = Script {
        breaks_after: Some(1),
        ..Script::honest(&sample)
    };
    let mut setup = Setup::new(&sample);
    setup.options.requests_per_supplier = 1;
    let ran = setup.run(&[Made::new(1, one_at_a_time, &sample)]);

    assert!(ran.has_every_file(&sample), "{:?}", ran.outcome);
    // Seven chunks, with the shortest wait after each. Waits that doubled
    // would come to two minutes.
    assert!(ran.took < Duration::from_secs(30), "it took {:?}", ran.took);
}

/// A supplier that cannot be reached is tried less and less often.
#[test]
fn a_supplier_that_cannot_be_reached_is_tried_less_and_less_often() {
    let sample = sample();
    let gone = Script {
        unreachable_for: usize::MAX,
        ..Script::honest(&sample)
    };
    let supplier = Made::new(1, gone, &sample);
    let ran = Setup::new(&sample).run(std::slice::from_ref(&supplier));

    assert!(matches!(ran.outcome, Err(Error::Stalled(_))));
    // At 0, 2, 6, 14 and 30 seconds, then every 30: a handful of times in
    // a minute and a half, where every two seconds would be 45.
    let attempts = supplier.0.opened.load(Ordering::Relaxed);
    assert!((5..=8).contains(&attempts), "{attempts} attempts");
}

/// A supplier with nothing to give makes way although others are busy:
/// what another is fetching is not its own work.
#[test]
fn an_idle_supplier_makes_way_while_another_is_fetching() {
    let sample = sample();
    let slow = Script {
        delay: Duration::from_secs(10),
        ..Script::honest(&sample)
    };
    let suppliers = [
        Made::new(1, Script::default(), &sample),
        Made::new(2, slow, &sample),
        Made::new(3, Script::honest(&sample), &sample),
    ];
    let mut setup = Setup::new(&sample);
    setup.options.max_suppliers = 2;
    let ran = setup.run(&suppliers);

    assert!(ran.has_every_file(&sample), "{:?}", ran.outcome);
    // The quick one got the idle one's turn and fetched what the slow one
    // had not begun. Only the chunks the slow one already had in hand took
    // its ten seconds: left to finish alone, it would have needed twenty.
    assert!(ran.stored_from(3) > 0);
    assert!(ran.took < Duration::from_secs(15), "it took {:?}", ran.took);
}

// The next three were found by the property test at the end of this file.
// Each made a transfer stall although a supplier that could finish it was
// known.

/// A supplier with nothing to give does not keep the only turn there is.
#[test]
fn a_supplier_with_nothing_wanted_makes_way_for_one_that_has() {
    let sample = sample();
    let empty = Made::new(1, Script::default(), &sample);
    let full = Made::new(2, Script::honest(&sample), &sample);
    let mut setup = Setup::new(&sample);
    setup.options.max_suppliers = 1;
    let ran = setup.run(&[empty, full]);

    assert!(ran.has_every_file(&sample), "{:?}", ran.outcome);
    assert_eq!(ran.stored_from(2), sample.artifact.total_size());
}

/// A supplier is not made to give up its turn while it is fetching, even
/// when a fetch takes as long as the wait between two looks at it.
#[test]
fn a_slow_supplier_keeps_its_turn_while_it_is_fetching() {
    let sample = sample();
    let slow = Script {
        delay: Duration::from_secs(2),
        ..Script::honest(&sample)
    };
    let suppliers = [
        Made::new(1, slow, &sample),
        Made::new(2, Script::default(), &sample),
    ];
    let mut setup = Setup::new(&sample);
    setup.options.max_suppliers = 1;
    setup.options.requests_per_supplier = 1;
    let ran = setup.run(&suppliers);

    assert!(ran.has_every_file(&sample), "{:?}", ran.outcome);
}

/// A supplier that goes on offering a chunk it will not give does not keep
/// its turn for ever.
#[test]
fn a_supplier_that_offers_what_it_will_not_give_makes_way() {
    let sample = sample();
    let mut offered = vec![false; chunks(&sample)];
    offered[1] = true;
    let tease = Script {
        holds: offered.clone(),
        withholds: offered,
        ..Script::default()
    };
    let suppliers = [
        Made::new(1, tease, &sample),
        Made::new(2, Script::honest(&sample), &sample),
    ];
    let mut setup = Setup::new(&sample);
    setup.options.max_suppliers = 1;
    setup.options.requests_per_supplier = 1;
    let ran = setup.run(&suppliers);

    assert!(ran.has_every_file(&sample), "{:?}", ran.outcome);
    assert_eq!(ran.stored_from(1), 0);
}

/// Suppliers that take turns having nothing to give do not shut out one
/// that was known all along: whoever has waited longest goes first.
#[test]
fn suppliers_taking_turns_do_not_shut_out_another() {
    let sample = sample();
    let total = chunks(&sample);
    // Three that hold a chunk each, and one that holds everything but
    // cannot be reached at first.
    let mut suppliers: Vec<Made> = (0..3u8)
        .map(|n| {
            let mut holds = vec![false; total];
            holds[usize::from(n)] = true;
            let script = Script {
                holds,
                ..Script::default()
            };
            Made::new(n + 1, script, &sample)
        })
        .collect();
    let late = Script {
        unreachable_for: 1,
        ..Script::honest(&sample)
    };
    suppliers.push(Made::new(9, late, &sample));
    let mut setup = Setup::new(&sample);
    setup.options.max_suppliers = 1;
    let ran = setup.run(&suppliers);

    assert!(ran.has_every_file(&sample), "{:?}", ran.outcome);
}

/// Memory stays bounded: a supplier is asked for its next chunk only when
/// the store has taken the last, so a slow place holds the suppliers back.
#[test]
fn a_slow_place_holds_the_suppliers_back() {
    let sample = sample();
    // Chunks handed over by a supplier and not yet stored, and the most
    // there ever were.
    let in_hand = Arc::new(AtomicUsize::new(0));
    let most = Arc::new(AtomicUsize::new(0));
    let handed_over = {
        let (in_hand, most) = (in_hand.clone(), most.clone());
        Hook(Arc::new(move || {
            most.fetch_max(in_hand.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
        }))
    };
    let suppliers: Vec<Made> = (1..=3)
        .map(|n| Made::watched(n, Script::honest(&sample), &sample, handed_over.clone()))
        .collect();

    let mut setup = Setup::new(&sample);
    setup.options.max_suppliers = 2;
    setup.options.requests_per_supplier = 1;
    setup.before_write = Box::new(|| {
        std::thread::sleep(Duration::from_millis(5));
        Ok(())
    });
    setup.on_event = Box::new(move |event| {
        if matches!(event, Event::Stored { .. }) {
            in_hand.fetch_sub(1, Ordering::SeqCst);
        }
    });
    let ran = setup.run(&suppliers);

    assert!(ran.has_every_file(&sample));
    let most = most.load(Ordering::SeqCst);
    assert!(
        (1..=2).contains(&most),
        "{most} chunks were in hand at once"
    );
}

// From here on, the digests of the chunks are not known beforehand.

/// A supplier, and a count of the chunks it has handed over.
fn counted(number: u8, script: Script, sample: &Arc<Sample>) -> (Made, Arc<AtomicUsize>) {
    let count = Arc::new(AtomicUsize::new(0));
    let counting = count.clone();
    let hook = Hook(Arc::new(move || {
        counting.fetch_add(1, Ordering::SeqCst);
    }));
    (Made::watched(number, script, sample, hook), count)
}

/// What is known of each entry when a digest is given for every file that
/// has bytes. Nothing is given for a directory or an empty file.
fn with_file_digests(sample: &Sample, sha256: bool) -> Vec<Foreknown> {
    let digest = |bytes: &[u8]| {
        if sha256 {
            let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
            FileDigest::Sha256(digest.as_ref().try_into().unwrap())
        } else {
            FileDigest::Blake3(Hash::of(bytes))
        }
    };
    let known = |bytes: &Vec<u8>| Foreknown {
        validator: b"v1".to_vec(),
        digest: (!bytes.is_empty()).then(|| digest(bytes)),
    };
    sample.files.iter().map(known).collect()
}

/// A supplier that lacks two chunks, so that a transfer from it stalls with
/// two files unfinished and one committed.
fn lacking_two(sample: &Sample) -> Script {
    let mut most = Script::honest(sample);
    most.holds[0] = false;
    most.holds[5] = false;
    most
}

#[test]
fn digests_that_were_not_known_are_learned_as_the_chunks_arrive() {
    let sample = sample();
    let supplier = Made::new(1, Script::honest(&sample), &sample);
    let ran = Setup::learning(&sample, b"v1").run(&[supplier]);

    let summary = ran.outcome.as_ref().unwrap();
    assert!(ran.has_every_file(&sample));
    // Nothing vouched for the bytes but the source staying what it was.
    assert_eq!(summary.verification, Verification::Consistent);
    // What was learned is the artifact that the bytes are.
    assert_eq!(ran.store.artifact().as_deref(), Some(&*sample.artifact));
    // And nothing noted along the way is left beside a finished file.
    assert_eq!(ran.place.notes(1, 100), Ok(Vec::new()));
}

#[test]
fn a_digest_given_for_every_file_makes_the_transfer_verified() {
    for sha256 in [false, true] {
        let sample = sample();
        let mut setup = Setup::learning(&sample, b"v1");
        setup.foreknown = Some(with_file_digests(&sample, sha256));
        let ran = setup.run(&[Made::new(1, Script::honest(&sample), &sample)]);

        assert!(ran.has_every_file(&sample), "{:?}", ran.outcome);
        assert_eq!(ran.outcome.unwrap().verification, Verification::Verified);
    }
}

/// A digest for some files is not a digest for every file.
#[test]
fn a_file_with_bytes_and_no_digest_keeps_the_transfer_from_being_verified() {
    let sample = sample();
    let mut known = with_file_digests(&sample, false);
    // The last file is one byte long.
    known.last_mut().unwrap().digest = None;
    let mut setup = Setup::learning(&sample, b"v1");
    setup.foreknown = Some(known);
    let ran = setup.run(&[Made::new(1, Script::honest(&sample), &sample)]);

    assert!(ran.has_every_file(&sample), "{:?}", ran.outcome);
    assert_eq!(ran.outcome.unwrap().verification, Verification::Consistent);
}

/// With no digest for a chunk, a wrong chunk cannot be told from a right
/// one. The file it is in can, if a digest was given for the file.
#[test]
fn a_file_without_the_digest_given_for_it_is_thrown_away() {
    for sha256 in [false, true] {
        let sample = sample();
        let mut lies = vec![false; chunks(&sample)];
        lies[1] = true;
        let liar = Script {
            lies,
            ..Script::honest(&sample)
        };
        let mut setup = Setup::learning(&sample, b"v1");
        setup.foreknown = Some(with_file_digests(&sample, sha256));
        let ran = setup.run(&[Made::new(1, liar, &sample)]);

        let Err(Error::Place(Failure::Corrupt(reason))) = &ran.outcome else {
            panic!("{:?}", ran.outcome);
        };
        assert!(
            reason.contains("a.bin does not have the digest"),
            "{reason}"
        );
        let kind = if sha256 { "sha256:" } else { "blake3:" };
        assert!(reason.contains(&format!("expected {kind}")), "{reason}");
        assert!(reason.contains(&format!("found {kind}")), "{reason}");
        assert!(ran.has_no_wrong_file(&sample));
        // Nothing of it is left to be taken for the file later.
        assert_eq!(ran.place.committed(1), None);
        assert!(ran.place.read_back(1, 0, 1).is_err());
        assert_eq!(ran.place.notes(1, 100), Ok(Vec::new()));
    }
}

/// Found by the property test below. The supplier that completes the bad
/// file breaks off at that moment, so whoever was waiting to hear that the
/// file was thrown away is stopped first. The transfer must end with that
/// failure all the same: with every chunk marked done by another supplier,
/// it used to report success with the file missing.
#[test]
fn a_file_thrown_away_as_its_supplier_breaks_off_still_ends_the_transfer() {
    let sample = sample();
    let total = chunks(&sample);
    let has = |held: &[usize]| (0..total).map(|index| held.contains(&index)).collect();
    let honest = Script {
        holds: has(&[1, 2, 3, 6]),
        ..Script::default()
    };
    // The second file is chunks 4 and 5.
    let mut lies = vec![false; total];
    lies[4] = true;
    let liar = Script {
        holds: has(&[0, 4, 5]),
        lies,
        breaks_after: Some(1),
        ..Script::default()
    };
    let mut setup = Setup::learning(&sample, b"v1");
    setup.foreknown = Some(with_file_digests(&sample, false));
    let ran = setup.run(&[Made::new(1, honest, &sample), Made::new(2, liar, &sample)]);

    let thrown_away = matches!(&ran.outcome, Err(Error::Place(Failure::Corrupt(_))));
    assert!(thrown_away, "{:?}", ran.outcome);
    assert!(!ran.store.is_complete());
    assert_eq!(ran.place.committed(4), None);
    assert!(ran.has_no_wrong_file(&sample));
    // The store takes nothing more, having thrown part of itself away.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let later = runtime.block_on(ran.store.write_chunk(0, sample.chunk(0)));
    assert!(matches!(later, Err(Failure::Corrupt(_))), "{later:?}");
}

#[test]
fn an_interrupted_transfer_carries_on_from_what_it_noted() {
    let sample = sample();
    let first = Setup::learning(&sample, b"v1").run(&[Made::new(1, lacking_two(&sample), &sample)]);
    assert!(matches!(first.outcome, Err(Error::Stalled(_))));
    assert_eq!(
        first.store.artifact(),
        None,
        "not every digest is known yet"
    );

    let (supplier, handed_over) = counted(2, Script::honest(&sample), &sample);
    let second = Setup::learning(&sample, b"v1")
        .into(&first.place)
        .run(&[supplier]);
    assert!(second.has_every_file(&sample), "{:?}", second.outcome);
    assert_eq!(second.store.artifact().as_deref(), Some(&*sample.artifact));
    // The two chunks that were missing, and the one file that was already
    // finished: once a file is committed its notes are gone, so nothing
    // here says what it should be. Leaving a finished file alone is for
    // whoever plans the transfer, who can compare it with the source.
    assert_eq!(handed_over.load(Ordering::SeqCst), 3);
}

#[test]
fn notes_about_another_version_are_not_believed() {
    let sample = sample();
    let first = Setup::learning(&sample, b"v1").run(&[Made::new(1, lacking_two(&sample), &sample)]);
    assert!(matches!(first.outcome, Err(Error::Stalled(_))));

    let (supplier, handed_over) = counted(2, Script::honest(&sample), &sample);
    let second = Setup::learning(&sample, b"v2")
        .into(&first.place)
        .run(&[supplier]);
    assert!(second.has_every_file(&sample), "{:?}", second.outcome);
    assert_eq!(handed_over.load(Ordering::SeqCst), chunks(&sample));
}

#[test]
fn with_nothing_to_tell_versions_apart_nothing_is_carried_over() {
    let sample = sample();
    let first = Setup::learning(&sample, b"").run(&[Made::new(1, lacking_two(&sample), &sample)]);
    assert!(matches!(first.outcome, Err(Error::Stalled(_))));
    assert_eq!(first.place.notes(1, 100), Ok(Vec::new()), "notes were kept");

    let (supplier, handed_over) = counted(2, Script::honest(&sample), &sample);
    let second = Setup::learning(&sample, b"")
        .into(&first.place)
        .run(&[supplier]);
    assert!(second.has_every_file(&sample), "{:?}", second.outcome);
    assert_eq!(handed_over.load(Ordering::SeqCst), chunks(&sample));
}

#[test]
fn a_chunk_damaged_since_it_was_noted_is_fetched_again() {
    let sample = sample();
    let first = Setup::learning(&sample, b"v1").run(&[Made::new(1, lacking_two(&sample), &sample)]);
    assert!(matches!(first.outcome, Err(Error::Stalled(_))));
    // Chunk 1 is the second chunk of the first file.
    let within = sample.artifact.locate(1).unwrap();
    first.place.write(1, within.offset + 7, b"damage").unwrap();

    let (supplier, handed_over) = counted(2, Script::honest(&sample), &sample);
    let second = Setup::learning(&sample, b"v1")
        .into(&first.place)
        .run(&[supplier]);
    assert!(second.has_every_file(&sample), "{:?}", second.outcome);
    // As when nothing was damaged, and the damaged chunk.
    assert_eq!(handed_over.load(Ordering::SeqCst), 4);
}

#[test]
fn a_source_that_changes_ends_the_transfer_and_leaves_nothing_half_written() {
    let sample = sample();
    let changing = Script {
        changes_after: Some(2),
        ..Script::honest(&sample)
    };
    let mut setup = Setup::learning(&sample, b"v1");
    setup.options.requests_per_supplier = 1;
    let ran = setup.run(&[Made::new(1, changing, &sample)]);

    let Err(Error::Changed(what)) = &ran.outcome else {
        panic!("{:?}", ran.outcome);
    };
    assert_eq!(what, "the file is not what it was");
    assert!(ran.has_no_wrong_file(&sample));
    for (file, entry) in sample.artifact.entries().iter().enumerate() {
        if entry.size() > 0 {
            assert!(
                ran.place.read_back(file, 0, 1).is_err(),
                "file {file} is half written"
            );
            assert_eq!(ran.place.notes(file, 100), Ok(Vec::new()));
        }
    }
}

#[tokio::test]
async fn with_no_digest_bytes_of_the_wrong_length_are_still_refused() {
    let sample = sample();
    let place = Arc::new(Memory::new(sample.artifact.tree().clone()));
    let tree = sample.artifact.tree().clone();
    // Nothing is said of any entry, which is as good as nothing known.
    let store = Store::learn(tree, place, Vec::new(), |_| {}).await.unwrap();
    assert_eq!(store.verification(), Verification::Consistent);

    let mut short = sample.chunk(0);
    short.pop();
    assert_eq!(store.write_chunk(0, short).await, Ok(crate::Put::Refused));
    assert_eq!(
        store.write_chunk(0, Vec::new()).await,
        Ok(crate::Put::Refused)
    );
    assert!(!store.has(0));
    assert_eq!(
        store.write_chunk(0, sample.chunk(0)).await,
        Ok(crate::Put::Stored)
    );
    assert_eq!(store.artifact(), None);
}

// From here on, the supplier is a source: a place that is read in order.

/// A place that holds the whole sample, and counts how it is read.
#[derive(Debug)]
struct Watched {
    place: Memory,
    /// Whether any range may be asked for, or only a stream from the start.
    random_access: bool,
    /// The file and offset of every reader that was opened.
    opened: Mutex<Vec<(usize, u64)>>,
}

impl Watched {
    fn new(sample: &Arc<Sample>, random_access: bool) -> Arc<Self> {
        let filled = Setup::new(sample).run(&[Made::new(1, Script::honest(sample), sample)]);
        assert!(filled.has_every_file(sample));
        Arc::new(Self {
            place: filled.place,
            random_access,
            opened: Mutex::default(),
        })
    }

    fn supplier(self: &Arc<Self>, sample: &Sample) -> Vec<Arc<dyn Supplier>> {
        vec![crate::supplier(
            [9; 32],
            sample.artifact.tree().clone(),
            self.clone(),
        )]
    }
}

impl Source for Watched {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            random_access: self.random_access,
        }
    }

    fn describe(&self) -> Pending<'_, Description> {
        self.place.describe()
    }

    fn open(&self, file: usize, offset: u64) -> Pending<'_, Box<dyn Reader>> {
        self.opened.lock().unwrap().push((file, offset));
        self.place.open(file, offset)
    }
}

#[test]
fn a_source_supplies_its_chunks() {
    let sample = sample();
    let source = Watched::new(&sample, true);
    let ran = Setup::new(&sample).run_from(source.supplier(&sample));

    assert!(ran.has_every_file(&sample), "{:?}", ran.outcome);
    assert_eq!(ran.stored_from(9), sample.artifact.total_size());
    assert_eq!(ran.outcome.unwrap().verification, Verification::Verified);
}

/// A chunk that follows the one before it is read on from the same reader,
/// so a run of chunks is one opening and not one for each.
#[test]
fn chunks_in_a_row_come_from_one_reader() {
    let sample = sample();
    let source = Watched::new(&sample, true);
    let mut setup = Setup::new(&sample);
    setup.options.requests_per_supplier = 1;
    let ran = setup.run_from(source.supplier(&sample));

    assert!(ran.has_every_file(&sample), "{:?}", ran.outcome);
    // One pipeline goes once round the chunks from wherever it starts. It
    // needs a new reader where a file begins, and where it comes back
    // round to the start: the three files, and at most once more.
    let opened = source.opened.lock().unwrap().len();
    assert!(
        (3..=4).contains(&opened),
        "{opened} readers for seven chunks"
    );
}

/// A source that is one stream is read from its start, each file once, by
/// one pipeline, however many are asked for.
#[test]
fn a_source_that_is_one_stream_is_read_in_order() {
    let sample = sample();
    let source = Watched::new(&sample, false);
    let mut setup = Setup::learning(&sample, b"v1");
    setup.options.requests_per_supplier = 4;
    let ran = setup.run_from(source.supplier(&sample));

    assert!(ran.has_every_file(&sample), "{:?}", ran.outcome);
    assert_eq!(*source.opened.lock().unwrap(), [(1, 0), (4, 0), (5, 0)]);
    let stored = ran
        .events
        .iter()
        .filter(|e| matches!(e, Event::Stored { .. }));
    assert_eq!(stored.count(), chunks(&sample));
    let learned = ran.store.artifact();
    assert_eq!(learned.as_deref(), Some(&*sample.artifact));
}

/// A made-up supplier that is one stream is asked for its chunks in order.
#[test]
fn a_supplier_that_is_one_stream_is_asked_in_order() {
    let sample = sample();
    let stream = Script {
        in_order: true,
        ..Script::honest(&sample)
    };
    let supplier = Made::new(1, stream, &sample);
    let ran = Setup::new(&sample).run(std::slice::from_ref(&supplier));

    assert!(ran.has_every_file(&sample), "{:?}", ran.outcome);
    let asked = supplier.0.asked.lock().unwrap().clone();
    assert_eq!(
        asked,
        (0..sample.artifact.chunk_count()).collect::<Vec<_>>()
    );
}

fn script(chunks: usize) -> impl Strategy<Value = Script> {
    let flags = move |likely: f64| vec(proptest::bool::weighted(likely), chunks);
    (
        flags(0.8),
        flags(0.1),
        flags(0.1),
        proptest::option::weighted(0.3, 0usize..4),
        0u64..3000,
        0usize..3,
    )
        .prop_map(
            |(holds, lies, withholds, breaks_after, delay, unreachable_for)| Script {
                holds,
                lies,
                withholds,
                breaks_after,
                delay: Duration::from_millis(delay),
                unreachable_for,
                changes_after: None,
                in_order: false,
                others: Vec::new(),
            },
        )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    /// Whatever the suppliers do, no file has its final name unless it is
    /// the file that was sent. A transfer that succeeds has every file, and
    /// one supplier that does everything right is enough for it to succeed.
    #[test]
    fn a_transfer_ends_with_the_right_bytes_or_an_error(
        scripts in vec(script(7), 1..5),
        max_suppliers in 1usize..4,
        requests_per_supplier in 1usize..4,
    ) {
        let sample = sample();
        let total = sample.artifact.chunk_count();
        prop_assert_eq!(total, 7, "the scripts are written for seven chunks");
        let reliable = scripts.iter().any(|script| script.is_reliable(total));
        let suppliers: Vec<Made> = scripts
            .into_iter()
            .zip(1u8..)
            .map(|(script, number)| Made::new(number, script, &sample))
            .collect();
        let mut setup = Setup::new(&sample);
        setup.options.max_suppliers = max_suppliers;
        setup.options.requests_per_supplier = requests_per_supplier;
        let ran = setup.run(&suppliers);

        prop_assert!(ran.has_no_wrong_file(&sample), "a wrong file was committed");
        match &ran.outcome {
            Ok(summary) => {
                prop_assert!(ran.store.is_complete());
                prop_assert!(ran.has_every_file(&sample));
                let stored: u64 = summary.sources.iter().map(|(_, bytes)| bytes).sum();
                prop_assert_eq!(stored, sample.artifact.total_size());
            }
            Err(error) => {
                prop_assert!(!reliable, "a reliable supplier was there, yet: {}", error);
                prop_assert!(matches!(error, Error::Stalled(_)), "{}", error);
            }
        }
    }

    /// The same, when the digests are learned. With a digest given for
    /// every file, a liar cannot get a wrong file committed. With none, a
    /// transfer from suppliers that do not lie learns the artifact that the
    /// bytes are.
    #[test]
    fn a_transfer_that_learns_ends_with_the_right_bytes_or_an_error(
        mut scripts in vec(script(7), 1..4),
        digests_given in any::<bool>(),
        sha256 in any::<bool>(),
    ) {
        let sample = sample();
        if !digests_given {
            for script in &mut scripts {
                script.lies.fill(false);
            }
        }
        let reliable = scripts.iter().any(|script| script.is_reliable(7));
        let lies = scripts.iter().any(|script| script.lies.contains(&true));
        let suppliers: Vec<Made> = scripts
            .into_iter()
            .zip(1u8..)
            .map(|(script, number)| Made::new(number, script, &sample))
            .collect();
        let mut setup = Setup::learning(&sample, b"v1");
        if digests_given {
            setup.foreknown = Some(with_file_digests(&sample, sha256));
        }
        let ran = setup.run(&suppliers);

        prop_assert!(ran.has_no_wrong_file(&sample), "a wrong file was committed");
        match &ran.outcome {
            Ok(summary) => {
                prop_assert!(ran.has_every_file(&sample));
                let expected = if digests_given {
                    Verification::Verified
                } else {
                    Verification::Consistent
                };
                prop_assert_eq!(summary.verification, expected);
                let learned = ran.store.artifact();
                prop_assert_eq!(learned.as_deref(), Some(&*sample.artifact));
            }
            Err(Error::Stalled(_)) => prop_assert!(!reliable || lies, "it stalled"),
            // A liar's chunk got into a file, which was then found out.
            Err(Error::Place(Failure::Corrupt(_))) => prop_assert!(lies),
            Err(error) => prop_assert!(false, "{}", error),
        }
    }
}
