//! Transfers from suppliers that misbehave.
//!
//! The suppliers here are made up: each follows a script that says what it
//! holds, when it lies, when it breaks off and how slow it is. Whatever the
//! scripts, a transfer must end with the right bytes or with an error, and
//! never with a wrong file. Time is the test's own, so a minute of waiting
//! takes no time at all.

use crate::conformance::{Memory, Sample};
use crate::{Error, Options, Store, Summary, transfer};
use model::artifact::{Bitfield, FileDigest, Hash, MIN_CHUNK_SIZE};
use model::pace;
use model::place::{
    Arrived, Begun, Capabilities, Description, Event, Expected, Failure, Foreknown, Observer,
    Pending, Reader, Sink, Source, Status, Supplier, SupplierId, Supply, Verification,
};
use proptest::collection::vec;
use proptest::prelude::*;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
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
    /// It counts the bytes of a chunk as they come, a tenth of the chunk in
    /// each tenth of that time. Otherwise it counts nothing until the
    /// chunk is whole, like a place that cannot tell.
    steadily: bool,
    /// How many times it cannot be reached before it can.
    unreachable_for: usize,
    /// After this many chunks, what it reads from has changed.
    changes_after: Option<usize>,
    /// After this many chunks, it says it is of no use at all.
    useless_after: Option<usize>,
    /// It is one stream: it gives its chunks only in order, one at a time.
    in_order: bool,
    /// Every request to it travels on one connection.
    multiplexed: bool,
    /// Its line carries one chunk at a time, in the order they were asked
    /// for, and takes this long over each.
    line: Duration,
    /// After this many chunks, its line takes ten times as long.
    tires_after: Option<usize>,
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
    /// Held by the request whose chunk is on the line.
    line: tokio::sync::Mutex<()>,
    /// How many requests are in flight.
    in_flight: AtomicUsize,
    /// How many were in flight, each time it was asked for a chunk.
    in_flight_when_asked: Mutex<Vec<usize>>,
}

/// A request in flight, counted for as long as this lives.
struct Asking<'a>(&'a Inner);

impl<'a> Asking<'a> {
    fn begin(inner: &'a Inner, index: u64) -> Self {
        let in_flight = inner.in_flight.fetch_add(1, Ordering::Relaxed) + 1;
        inner.asked.lock().unwrap().push(index);
        inner.in_flight_when_asked.lock().unwrap().push(in_flight);
        Self(inner)
    }
}

impl Drop for Asking<'_> {
    fn drop(&mut self) {
        self.0.in_flight.fetch_sub(1, Ordering::Relaxed);
    }
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
            line: tokio::sync::Mutex::default(),
            in_flight: AtomicUsize::new(0),
            in_flight_when_asked: Mutex::default(),
        }))
    }

    /// How many requests were in flight, each time it was asked for a
    /// chunk, that request among them.
    fn in_flight_when_asked(&self) -> Vec<usize> {
        self.0.in_flight_when_asked.lock().unwrap().clone()
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

    fn chunk<'a>(
        &'a self,
        index: u64,
        _following: u64,
        arrived: &'a Arrived,
    ) -> Pending<'a, Vec<u8>> {
        Box::pin(async move {
            let script = &self.made.script;
            let _asking = Asking::begin(&self.made, index);
            if script.steadily {
                let tenth = self.made.sample.chunk(index).len() / 10;
                for _ in 0..10 {
                    tokio::time::sleep(script.delay / 10).await;
                    arrived.add(tenth);
                }
            } else {
                tokio::time::sleep(script.delay).await;
            }
            if !script.line.is_zero() {
                let _line = self.made.line.lock().await;
                let tired = script
                    .tires_after
                    .is_some_and(|limit| self.sent.load(Ordering::Relaxed) >= limit);
                tokio::time::sleep(script.line * if tired { 10 } else { 1 }).await;
            }
            let sent = self.sent.fetch_add(1, Ordering::Relaxed);
            if script.breaks_after.is_some_and(|limit| sent >= limit) {
                return Err(Failure::Unreachable("the line went dead".into()));
            }
            if script.changes_after.is_some_and(|limit| sent >= limit) {
                return Err(Failure::Changed("the file is not what it was".into()));
            }
            if script.useless_after.is_some_and(|limit| sent >= limit) {
                return Err(Failure::Fatal("it wants a login".into()));
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

    fn leave(&self, _index: u64) {}

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            random_access: !self.made.script.in_order,
            multiplexed: self.made.script.multiplexed,
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

    fn open(&self, file: usize, offset: u64, len: u64) -> Pending<'_, Box<dyn Reader>> {
        self.place.open(file, offset, len)
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
    ) -> Result<Begun, Failure> {
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
            // Four requests at a time to a supplier: what the tests here
            // count on, where they count.
            options: Options {
                requests_per_supplier: 4,
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
    // One for each file. The empty one needed nothing fetched and was
    // named as the place was made ready, so it is told of before the rest.
    let mut committed: Vec<usize> = ran
        .events
        .iter()
        .filter_map(|event| match event {
            Event::Committed { file } => Some(*file),
            _ => None,
        })
        .collect();
    assert_eq!(committed[0], 2);
    committed.sort_unstable();
    assert_eq!(committed, [1, 2, 4, 5]);
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

/// A supplier that was dropped is never tried again. Once every one has
/// been, nobody is left who could deliver, and the transfer says so at
/// once, with what the last one was dropped for.
#[test]
fn with_every_supplier_dropped_the_transfer_ends_at_once_and_says_why() {
    let sample = sample();
    let liar = Script {
        lies: vec![true; chunks(&sample)],
        ..Script::honest(&sample)
    };
    let useless = Script {
        useless_after: Some(2),
        ..Script::honest(&sample)
    };
    let both = [
        Made::new(1, liar.clone(), &sample),
        Made::new(2, liar.clone(), &sample),
    ];
    for (suppliers, why) in [
        (&[Made::new(1, liar, &sample)][..], "failed verification"),
        (&both[..], "failed verification"),
        (&[Made::new(1, useless, &sample)][..], "it wants a login"),
        (&[][..], "no supplier was given"),
    ] {
        let mut setup = Setup::new(&sample);
        setup.options.requests_per_supplier = 1;
        let ran = setup.run(suppliers);

        let Err(Error::Unsupplied(reason)) = &ran.outcome else {
            panic!("{:?}", ran.outcome);
        };
        assert!(reason.contains(why), "{reason}");
        // Not after the ninety seconds a stall is given.
        assert!(ran.took < Duration::from_secs(1), "it took {:?}", ran.took);
        assert!(ran.has_no_wrong_file(&sample));
    }
}

/// A chunk that has arrived is seen through before the transfer gives up,
/// though the supplier it came from was dropped in the meantime.
#[test]
fn a_chunk_being_stored_is_waited_for_when_the_last_supplier_is_dropped() {
    let sample = sample();
    // It holds two chunks, sends both at once, and one of them is a lie.
    let mut script = Script::honest(&sample);
    script.holds = vec![false; chunks(&sample)];
    script.holds[..2].fill(true);
    script.lies = vec![false, true];
    let mut setup = Setup::new(&sample);
    setup.options.requests_per_supplier = 2;
    // Writing takes long enough that the lie is found out first: it is
    // refused before anything is written.
    setup.before_write = Box::new(|| {
        std::thread::sleep(Duration::from_millis(200));
        Ok(())
    });
    let ran = setup.run(&[Made::new(1, script, &sample)]);

    let Err(Error::Unsupplied(reason)) = &ran.outcome else {
        panic!("{:?}", ran.outcome);
    };
    assert!(reason.contains("chunk 1 failed verification"), "{reason}");
    // The chunk that was right was stored, and counted, before that.
    assert_eq!(ran.store.held().count_ones(), 1);
    assert_eq!(ran.count(|e| matches!(e, Event::Stored { .. })), 1);
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

/// Where every chunk has its digest, a source that changes has spoiled
/// nothing: it is dropped as one that sends wrong bytes is, what it sent
/// before is kept, and another supplier finishes.
#[test]
fn a_source_that_changes_is_only_dropped_where_every_chunk_has_its_digest() {
    let sample = sample();
    let changing = Script {
        changes_after: Some(2),
        ..Script::honest(&sample)
    };
    let suppliers = [
        Made::new(1, changing.clone(), &sample),
        Made::new(2, Script::honest(&sample), &sample),
    ];
    let ran = Setup::new(&sample).run(&suppliers);
    assert!(ran.has_every_file(&sample), "{:?}", ran.outcome);
    let summary = ran.outcome.as_ref().unwrap();
    assert_eq!(summary.usable, [[2; 32]]);

    // By itself it cannot finish, and what it sent is still there for a
    // transfer that carries on.
    let mut setup = Setup::new(&sample);
    setup.options.requests_per_supplier = 1;
    let alone = setup.run(&[Made::new(1, changing, &sample)]);
    let Err(Error::Unsupplied(why)) = &alone.outcome else {
        panic!("{:?}", alone.outcome);
    };
    assert_eq!(why, "the file is not what it was");
    assert!(alone.has_no_wrong_file(&sample));
    let carried_on = Setup::new(&sample).into(&alone.place).run(&[Made::new(
        2,
        Script::honest(&sample),
        &sample,
    )]);
    assert!(carried_on.has_every_file(&sample));
    let kept = carried_on.count(|event| matches!(event, Event::Stored { .. }));
    assert_eq!(kept, chunks(&sample) - 2);
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
    /// How many bytes the readers were opened for, all told.
    asked: AtomicU64,
    /// How many of those bytes were never read.
    unread: Arc<AtomicU64>,
    /// How many readers are open, and the most there have been at once.
    open: Arc<AtomicUsize>,
    most_open: AtomicUsize,
    /// The most readers it lets be open at once, if it has a limit, and
    /// how many it has refused for being one more.
    crowd: Option<usize>,
    refused: AtomicUsize,
    /// How long a reader takes over each read.
    each: Duration,
    /// A reader opened at this offset takes this long instead.
    slow: Option<(u64, Duration)>,
}

/// A reader that is counted as open for as long as it lives.
#[derive(Debug)]
struct Open {
    reader: Box<dyn Reader>,
    /// How many readers there are.
    readers: Arc<AtomicUsize>,
    /// How many of the bytes it was opened for have not been read.
    left: u64,
    unread: Arc<AtomicU64>,
    delay: Duration,
}

impl Reader for Open {
    fn read<'a>(&'a mut self, len: u32, arrived: &'a Arrived) -> Pending<'a, Vec<u8>> {
        Box::pin(async move {
            tokio::time::sleep(self.delay).await;
            let read = self.reader.read(len, arrived).await?;
            self.left = self.left.saturating_sub(read.len() as u64);
            Ok(read)
        })
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        self.readers.fetch_sub(1, Ordering::Relaxed);
        self.unread.fetch_add(self.left, Ordering::Relaxed);
    }
}

impl Watched {
    fn new(sample: &Arc<Sample>, random_access: bool) -> Arc<Self> {
        let tree = sample.artifact.tree().clone();
        Arc::new(Self {
            place: Memory::holding(tree, &sample.files).unwrap(),
            random_access,
            opened: Mutex::default(),
            asked: AtomicU64::new(0),
            unread: Arc::default(),
            open: Arc::default(),
            most_open: AtomicUsize::new(0),
            crowd: None,
            refused: AtomicUsize::new(0),
            each: Duration::ZERO,
            slow: None,
        })
    }

    /// The same, with every read taking `each`, and no more than `most`
    /// readers open at once: one more is refused.
    fn crowded(sample: &Arc<Sample>, each: Duration, most: usize) -> Arc<Self> {
        let mut watched = Arc::into_inner(Self::new(sample, true)).unwrap();
        (watched.each, watched.crowd) = (each, Some(most));
        Arc::new(watched)
    }

    /// The same, with every read taking `each`, but `slow.1` from a reader
    /// that was opened at the offset `slow.0`.
    fn taking(sample: &Arc<Sample>, each: Duration, slow: Option<(u64, Duration)>) -> Arc<Self> {
        let mut watched = Arc::into_inner(Self::new(sample, true)).unwrap();
        (watched.each, watched.slow) = (each, slow);
        Arc::new(watched)
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
            multiplexed: false,
        }
    }

    fn describe(&self) -> Pending<'_, Description> {
        self.place.describe()
    }

    fn open(&self, file: usize, offset: u64, len: u64) -> Pending<'_, Box<dyn Reader>> {
        // One reader too many is refused, as a server refuses one
        // connection too many.
        let open = self.open.fetch_add(1, Ordering::Relaxed) + 1;
        if self.crowd.is_some_and(|most| open > most) {
            self.open.fetch_sub(1, Ordering::Relaxed);
            self.refused.fetch_add(1, Ordering::Relaxed);
            let busy = Failure::Unreachable("too many readers at once".into());
            return Box::pin(std::future::ready(Err(busy)));
        }
        self.opened.lock().unwrap().push((file, offset));
        self.asked.fetch_add(len, Ordering::Relaxed);
        Box::pin(async move {
            let reader = self.place.open(file, offset, len).await?;
            self.most_open.fetch_max(open, Ordering::Relaxed);
            let slow = self.slow.filter(|(slow, _)| *slow == offset);
            let counted: Box<dyn Reader> = Box::new(Open {
                reader,
                readers: self.open.clone(),
                left: len,
                unread: self.unread.clone(),
                delay: slow.map_or(self.each, |(_, delay)| delay),
            });
            Ok(counted)
        })
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

/// A request is for a run of chunks and ends where the run ends. So with
/// streams that keep their speed, every byte a source was opened for is
/// read, and each stream opens it a few times and no more.
#[test]
fn nothing_is_asked_for_that_is_not_read() {
    let sample = long(64);
    let source = Watched::taking(&sample, Duration::from_millis(10), None);
    let mut setup = Setup::new(&sample);
    setup.options.requests_per_supplier = 4;
    let ran = setup.run_from(source.supplier(&sample));

    assert!(ran.has_every_file(&sample), "{:?}", ran.outcome);
    let size = sample.artifact.total_size();
    assert_eq!(source.asked.load(Ordering::Relaxed), size);
    assert_eq!(source.unread.load(Ordering::Relaxed), 0);
    assert!(source.most_open.load(Ordering::Relaxed) <= 4);
    // A first run each, to learn how fast the stream is, and then its
    // share of the rest: eight, and a few more for what the shares left.
    let opened = source.opened.lock().unwrap().len();
    assert!((8..=12).contains(&opened), "{opened} readers");
    // Sixty-four chunks at a hundredth of a second each, by four streams.
    assert!(
        ran.took < Duration::from_millis(200),
        "it took {:?}",
        ran.took
    );
}

/// One of four streams takes a second over each chunk where the others
/// take a hundredth. When they have fetched everything else, they take over what
/// the slow one has not come to, half of it at a time, and the copy does
/// not wait for the slow one to read all it had taken.
#[test]
fn a_stream_that_falls_behind_is_relieved() {
    let sample = long(64);
    // The reader opened at the start of the file is the slow one.
    let slow = Some((0, Duration::from_secs(1)));
    let source = Watched::taking(&sample, Duration::from_millis(10), slow);
    let mut setup = Setup::new(&sample);
    setup.options.requests_per_supplier = 4;
    let ran = setup.run_from(source.supplier(&sample));

    assert!(ran.has_every_file(&sample), "{:?}", ran.outcome);
    // Its first run is a sixteenth of the file: eight chunks, and eight
    // seconds if it read them all.
    assert!(ran.took < Duration::from_secs(6), "it took {:?}", ran.took);
    assert!(ran.took >= Duration::from_secs(2), "it took {:?}", ran.took);
    // What it was opened for and did not read was read by the others.
    assert!(source.unread.load(Ordering::Relaxed) > 0);
    assert!(source.most_open.load(Ordering::Relaxed) <= 4);
}

/// A source that takes two readers at a time and refuses a third is used
/// with two: the requests it refuses step aside, and the transfer goes on
/// without reaching for the source again.
#[test]
fn a_source_that_takes_fewer_requests_is_used_with_those_it_takes() {
    let sample = long(64);
    let source = Watched::crowded(&sample, Duration::from_millis(10), 2);
    let mut setup = Setup::new(&sample);
    setup.options.requests_per_supplier = 8;
    let ran = setup.run_from(source.supplier(&sample));

    assert!(ran.has_every_file(&sample), "{:?}", ran.outcome);
    assert_eq!(source.most_open.load(Ordering::Relaxed), 2);
    assert!(source.refused.load(Ordering::Relaxed) >= 6);
    // Sixty-four chunks at a hundredth of a second each, by two streams,
    // with no waiting to try the source again.
    assert!(
        ran.took < Duration::from_millis(500),
        "it took {:?}",
        ran.took
    );
    let closed = |event: &&Event| matches!(event, Event::Closed { .. });
    assert_eq!(ran.events.iter().filter(closed).count(), 1);
}

/// A source that refuses every reader fails as a supplier: the last of
/// its requests does not step aside.
#[test]
fn a_source_that_takes_no_request_is_a_supplier_that_failed() {
    let sample = long(8);
    let source = Watched::crowded(&sample, Duration::ZERO, 0);
    let mut setup = Setup::new(&sample);
    setup.options.requests_per_supplier = 8;
    setup.options.stall_timeout = Duration::from_secs(20);
    let ran = setup.run_from(source.supplier(&sample));

    assert!(
        matches!(ran.outcome, Err(Error::Stalled(_))),
        "{:?}",
        ran.outcome
    );
    // It was reached for again, after waits that double.
    let opened = |event: &&Event| matches!(event, Event::Opened { .. });
    let attempts = ran.events.iter().filter(opened).count();
    assert!((3..=8).contains(&attempts), "{attempts} attempts");
}

/// A reader is kept open for the chunks that are to follow, and closed
/// when its last has been read or when it is said that they will not be
/// asked for. Left open, a request goes on being sent what nobody reads.
#[test]
fn a_reader_is_open_for_as_long_as_it_will_be_read() {
    let sample = long(8);
    let source = Watched::new(&sample, true);
    let supplier = source.supplier(&sample).remove(0);
    let chunk = u64::from(MIN_CHUNK_SIZE);
    let open = || source.open.load(Ordering::Relaxed);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    runtime.block_on(async {
        let supply = supplier.open().await.unwrap();
        let arrived = Arrived::default();
        // Opened for a chunk and the three that are to follow.
        supply.chunk(0, 3, &arrived).await.unwrap();
        assert_eq!(source.asked.load(Ordering::Relaxed), 4 * chunk);
        assert_eq!(open(), 1);
        // The next is read on from it, whatever is now said to follow.
        supply.chunk(1, 0, &arrived).await.unwrap();
        assert_eq!(source.opened.lock().unwrap().len(), 1);
        assert_eq!(open(), 1);
        // The one after that will not be asked for.
        supply.leave(2);
        assert_eq!(open(), 0);
        assert_eq!(source.unread.load(Ordering::Relaxed), 2 * chunk);

        // A run that is read to its end closes its reader by itself.
        supply.chunk(4, 1, &arrived).await.unwrap();
        assert_eq!(open(), 1);
        supply.chunk(5, 0, &arrived).await.unwrap();
        assert_eq!(open(), 0);
        assert_eq!(source.unread.load(Ordering::Relaxed), 2 * chunk);
        // And a run does not reach past the end of its file.
        supply.chunk(7, 5, &arrived).await.unwrap();
        assert_eq!(open(), 0);
        assert_eq!(source.asked.load(Ordering::Relaxed), 7 * chunk);
    });
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

/// Three chunks of a size that takes a while to come, for what only shows
/// when a chunk is slow.
fn large() -> Arc<Sample> {
    let chunk = 256 * 1024;
    let tree = [("", None), ("large.bin", Some(3 * chunk))];
    Arc::new(Sample::with_chunks(&tree, u32::try_from(chunk).unwrap()).unwrap())
}

/// A supplier whose chunks take longer than a request with no sign of life
/// is given, and longer than a transfer goes without progress, is waited
/// for as long as the bytes keep coming.
#[test]
fn a_supplier_that_is_slow_and_keeps_sending_is_waited_for() {
    let sample = large();
    let slow = Script {
        delay: Duration::from_secs(150),
        steadily: true,
        ..Script::honest(&sample)
    };
    let supplier = Made::new(1, slow, &sample);
    let ran = Setup::new(&sample).run(std::slice::from_ref(&supplier));

    assert!(ran.has_every_file(&sample), "{:?}", ran.outcome);
    assert!(
        ran.took >= Duration::from_secs(150),
        "it took {:?}",
        ran.took
    );
    assert_eq!(supplier.0.opened.load(Ordering::Relaxed), 1);
}

/// The same supplier, if it does not say what has come until a chunk is
/// whole, cannot be told from one that sends nothing, and is given up.
#[test]
fn a_supplier_that_is_slow_and_gives_no_sign_is_given_up() {
    let sample = large();
    let silent = Script {
        delay: Duration::from_secs(150),
        ..Script::honest(&sample)
    };
    let supplier = Made::new(1, silent, &sample);
    let ran = Setup::new(&sample).run(&[supplier]);

    assert!(
        matches!(ran.outcome, Err(Error::Stalled(_))),
        "{:?}",
        ran.outcome
    );
}

/// A supplier that sends next to nothing is given up, even though bytes do
/// come: a tenth of a chunk in forty seconds is under a kilobyte a second.
#[test]
fn a_supplier_that_sends_next_to_nothing_is_given_up() {
    let sample = large();
    let trickle = Script {
        delay: Duration::from_secs(400),
        steadily: true,
        ..Script::honest(&sample)
    };
    let supplier = Made::new(1, trickle, &sample);
    let ran = Setup::new(&sample).run(std::slice::from_ref(&supplier));

    assert!(
        matches!(ran.outcome, Err(Error::Stalled(_))),
        "{:?}",
        ran.outcome
    );
    // Each request was given up, and its chunk asked for again by one of
    // the supplier's requests that was left.
    let asked = supplier.0.asked.lock().unwrap().len();
    assert!(asked > chunks(&sample), "asked for {asked} chunks");
}

/// One long file, for what only shows over many chunks.
fn long(chunks: usize) -> Arc<Sample> {
    let size = chunks * MIN_CHUNK_SIZE as usize;
    Arc::new(Sample::of(&[("", None), ("long.bin", Some(size))]).unwrap())
}

/// Whether each chunk asked for is the one after the chunk asked for before
/// it, going round from the last to the first.
fn is_in_a_row(asked: &[u64], chunks: u64) -> bool {
    asked
        .windows(2)
        .all(|pair| pair[1] == (pair[0] + 1) % chunks)
}

/// A supplier a round trip away with a fast line is asked for as many chunks
/// at once as there may be. With a set few, its line would stand idle for
/// most of every round trip: four at a time would take 14 seconds here.
#[test]
fn a_far_supplier_with_a_fast_line_is_asked_for_many_chunks_at_once() {
    let sample = long(400);
    let far = Script {
        multiplexed: true,
        delay: Duration::from_millis(100),
        line: Duration::from_millis(10),
        ..Script::honest(&sample)
    };
    let supplier = Made::new(1, far, &sample);
    let ran = Setup::new(&sample).run(std::slice::from_ref(&supplier));

    assert!(ran.has_every_file(&sample), "{:?}", ran.outcome);
    let in_flight = supplier.in_flight_when_asked();
    assert_eq!(in_flight.first(), Some(&1), "it starts with one request");
    let most = usize::try_from(pace::most(u64::from(MIN_CHUNK_SIZE))).unwrap();
    assert_eq!(in_flight.iter().max(), Some(&most));
    assert!(ran.took < Duration::from_secs(7), "it took {:?}", ran.took);
    // All of them take the chunks in a row between them.
    let asked = supplier.0.asked.lock().unwrap().clone();
    assert!(is_in_a_row(&asked, sample.artifact.chunk_count()));
}

/// The number of requests stops growing once a chunk no longer comes
/// quickly. Here each takes half a second on the line, so with four in
/// flight a chunk is two seconds in coming. The number doubles with each
/// round of answers, so it may pass that by as much again and no more.
#[test]
fn requests_stop_being_added_when_chunks_stop_being_quick() {
    let sample = long(120);
    let steady = Script {
        multiplexed: true,
        line: Duration::from_millis(500),
        ..Script::honest(&sample)
    };
    let supplier = Made::new(1, steady, &sample);
    let ran = Setup::new(&sample).run(std::slice::from_ref(&supplier));

    assert!(ran.has_every_file(&sample), "{:?}", ran.outcome);
    let most = supplier.in_flight_when_asked().into_iter().max().unwrap();
    assert!((4..=8).contains(&most), "{most} requests at once");
}

/// A supplier with a slow line is asked for one chunk at a time.
#[test]
fn a_supplier_with_a_slow_line_is_asked_for_one_chunk_at_a_time() {
    let sample = sample();
    let slow = Script {
        multiplexed: true,
        line: Duration::from_secs(3),
        ..Script::honest(&sample)
    };
    let supplier = Made::new(1, slow, &sample);
    let ran = Setup::new(&sample).run(std::slice::from_ref(&supplier));

    assert!(ran.has_every_file(&sample), "{:?}", ran.outcome);
    assert_eq!(supplier.in_flight_when_asked(), [1; 7]);
}

/// When a line slows down, the requests in flight come down to as many as
/// it still carries in good time. Here there come to be dozens at a tenth
/// of a second a chunk. At a second a chunk, every one of those is slow,
/// so they are taken away one by one, and then added again only for as
/// long as a chunk comes within two seconds.
#[test]
fn requests_are_taken_away_when_chunks_come_slowly() {
    let sample = long(600);
    let tiring = Script {
        multiplexed: true,
        line: Duration::from_millis(100),
        tires_after: Some(200),
        ..Script::honest(&sample)
    };
    let supplier = Made::new(1, tiring, &sample);
    let ran = Setup::new(&sample).run(std::slice::from_ref(&supplier));

    assert!(ran.has_every_file(&sample), "{:?}", ran.outcome);
    let in_flight = supplier.in_flight_when_asked();
    assert!(in_flight[..200].iter().max() >= Some(&20), "{in_flight:?}");
    let later = in_flight[300..500].iter().max().unwrap();
    assert!((2..=4).contains(later), "{in_flight:?}");
    assert_eq!(supplier.0.opened.load(Ordering::Relaxed), 1);
}

/// A supplier whose requests are each a connection is asked for the set
/// number at once, however quickly its chunks come.
#[test]
fn a_supplier_that_is_not_multiplexed_keeps_to_the_set_number() {
    let sample = long(400);
    let quick = Script {
        delay: Duration::from_millis(100),
        ..Script::honest(&sample)
    };
    let supplier = Made::new(1, quick, &sample);
    let mut setup = Setup::new(&sample);
    setup.options.requests_per_supplier = 3;
    let ran = setup.run(std::slice::from_ref(&supplier));

    assert!(ran.has_every_file(&sample), "{:?}", ran.outcome);
    assert_eq!(supplier.in_flight_when_asked().into_iter().max(), Some(3));
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
        proptest::bool::ANY,
        0u64..12_000,
    )
        .prop_map(
            |(holds, lies, withholds, breaks_after, delay, unreachable_for, multiplexed, line)| {
                Script {
                    holds,
                    lies,
                    withholds,
                    breaks_after,
                    delay: Duration::from_millis(delay),
                    unreachable_for,
                    changes_after: None,
                    useless_after: None,
                    in_order: false,
                    steadily: false,
                    multiplexed,
                    // Slow enough, at times, for there to be fewer requests.
                    line: Duration::from_millis(if multiplexed { line } else { 0 }),
                    tires_after: None,
                    others: Vec::new(),
                }
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
        let all_lie = scripts.iter().all(|script| script.lies.contains(&true));
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
                match error {
                    Error::Stalled(_) => {}
                    // Only a lie gets a supplier dropped here.
                    Error::Unsupplied(_) => prop_assert!(all_lie, "{}", error),
                    _ => prop_assert!(false, "{}", error),
                }
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
