//! Fetching what a store lacks from whoever can supply it.
//!
//! A transfer pulls the missing chunks from as many suppliers as it can
//! find. Every chunk is checked against the manifest before it is stored,
//! and a file gets its final name only when every one of its chunks is in
//! place. What a supplier is, a machine or a web server or a folder, is
//! nothing to this module.

use crate::claims::{Claim, Claims, Next, Reserved, Shared, Size};
use crate::store::{Put, Store};
use crate::sync;
use model::artifact::Bitfield;
use model::pace;
use model::place::{Arrived, Event, Failure, Observer, Supplier, SupplierId, Supply, Verification};
use model::retry;
use model::schedule::Owner;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use tokio::sync::{Notify, mpsc};
use tokio::task::JoinSet;
use tokio::time::{Instant, MissedTickBehavior, timeout};
use tracing::{debug, warn};

const OPEN_TIMEOUT: Duration = Duration::from_secs(10);
const STATUS_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a request may go without [`PROGRESS`] before it is given up.
const CHUNK_TIMEOUT: Duration = Duration::from_secs(60);
/// The bytes of one request that count as progress. A request that brings
/// fewer in [`CHUNK_TIMEOUT`] is given up, and a transfer in which no
/// request brings as many within the stall timeout has stalled. It comes
/// to about a kilobyte a second for each request, below which a link is
/// taken to carry nothing.
const PROGRESS: u64 = 64 * 1024;
/// How often a request is looked at to see what has arrived of it.
const WATCH: Duration = Duration::from_secs(1);
const STATUS_INTERVAL: Duration = Duration::from_secs(2);
const IDLE_POLL: Duration = Duration::from_secs(1);
/// How long a supplier that had nothing wanted is left alone.
const IDLE_WAIT: Duration = Duration::from_secs(2);
const MANAGER_TICK: Duration = Duration::from_millis(250);
/// Most suppliers a transfer will keep track of.
const MAX_KNOWN_SUPPLIERS: usize = 512;
/// Most suppliers accepted from a single status reply.
const MAX_OTHERS_PER_STATUS: usize = 64;

/// Limits for one transfer.
#[derive(Debug, Clone)]
pub struct Options {
    /// Suppliers to fetch from at the same time.
    pub max_suppliers: usize,
    /// The most requests made at the same time to each supplier whose
    /// requests are each a connection of their own. Each is for a run of
    /// chunks. A supplier that refuses some of them is used with those it
    /// takes. A supplier that takes every request on one connection is
    /// asked for as many chunks at once as its route has room for.
    pub requests_per_supplier: usize,
    /// Give up when next to nothing has arrived for this long.
    pub stall_timeout: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            max_suppliers: 8,
            requests_per_supplier: 8,
            stall_timeout: Duration::from_secs(60),
        }
    }
}

/// What a finished transfer did.
#[derive(Debug)]
pub struct Summary {
    /// Bytes stored from each supplier, largest first.
    pub sources: Vec<(SupplierId, u64)>,
    /// Every supplier the transfer came to know and did not drop for good.
    pub usable: Vec<SupplierId>,
    /// How far the bytes were checked.
    pub verification: Verification,
}

/// Why a transfer ended with chunks still missing.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// No chunk arrived for the whole of the stall timeout.
    #[error("the transfer stalled: no supplier made progress for {0:?}")]
    Stalled(Duration),

    /// What was being read changed while it was being read. Everything
    /// that had been written and not committed has been thrown away.
    #[error("{0}")]
    Changed(String),

    /// Nothing is left that could deliver: every supplier was dropped, for
    /// sending something wrong or for being of no use, or none was given.
    /// Holds why the last one was dropped.
    #[error("nothing is left to fetch from: {0}")]
    Unsupplied(String),

    /// The place being written to failed.
    #[error(transparent)]
    Place(#[from] Failure),
}

/// Fills `store` from `suppliers` and from those they tell of, and returns
/// once every chunk is stored. `me` tells this end from the suppliers, so
/// that it never fetches from itself. Each thing that happens is reported
/// to `observer`.
///
/// # Errors
///
/// Returns [`Error::Stalled`] if no chunk arrives within the stall timeout,
/// [`Error::Unsupplied`] as soon as every supplier has been dropped,
/// [`Error::Changed`] if a supplier finds that what it reads has changed,
/// and [`Error::Place`] if the store cannot keep what arrived.
pub async fn transfer(
    store: &Arc<Store>,
    me: SupplierId,
    suppliers: Vec<Arc<dyn Supplier>>,
    options: Options,
    observer: Observer,
) -> Result<Summary, Error> {
    let (found, mut discovered) = mpsc::unbounded_channel();
    let run = Run::new(store.clone(), me, options, observer, found);
    let mut slots: HashMap<SupplierId, Slot> = suppliers
        .into_iter()
        .map(|supplier| (supplier.id(), Slot::new(supplier)))
        .collect();
    let mut tasks: JoinSet<Ended> = JoinSet::new();
    let mut running: HashMap<tokio::task::Id, SupplierId> = HashMap::new();
    let mut ticker = tokio::time::interval(MANAGER_TICK);
    let mut last_dropped = String::from("no supplier was given");

    while !run.is_complete() {
        run.start_due(&mut slots, &mut tasks, &mut running);

        tokio::select! {
            Some(supplier) = discovered.recv() => {
                let (id, known) = (supplier.id(), slots.len());
                if id == me {
                    // A supplier may list us; there is nothing to do.
                } else if let Some(slot) = slots.get_mut(&id) {
                    if !slot.active {
                        // It may be reachable somewhere new.
                        slot.supplier = supplier;
                    }
                } else if known < MAX_KNOWN_SUPPLIERS {
                    debug!(supplier = %short(&id), "discovered a supplier");
                    slots.insert(id, Slot::new(supplier));
                }
            }
            Some(joined) = tasks.join_next_with_id() => {
                let (task, ended) = match joined {
                    Ok((task, ended)) => (task, ended),
                    Err(e) => (e.id(), Ended::Failed(format!("its task crashed: {e}"))),
                };
                let Some(id) = running.remove(&task) else { continue };
                let Some(slot) = slots.get_mut(&id) else { continue };
                slot.active = false;
                match ended {
                    Ended::Finished => {}
                    Ended::Idle => slot.retry_at = Instant::now() + IDLE_WAIT,
                    Ended::Failed(reason) => {
                        // A supplier that was delivering has failed once,
                        // not once more.
                        if run.stored_by(&id) > slot.stored_before {
                            slot.failures = 0;
                        }
                        slot.failures = slot.failures.saturating_add(1);
                        let wait = Duration::from_secs(retry::wait_seconds(slot.failures));
                        slot.retry_at = Instant::now() + wait;
                        debug!(supplier = %short(&id), "supplier failed ({reason}); retrying in {wait:?}");
                    }
                    Ended::Dropped(reason) => {
                        slot.dropped = true;
                        warn!(supplier = %short(&id), "dropping supplier: {reason}");
                        last_dropped = reason;
                    }
                    Ended::Fatal(Failure::Changed(what)) => {
                        run.store.abandon().await?;
                        return Err(Error::Changed(what));
                    }
                    Ended::Fatal(failure) => return Err(failure.into()),
                }
            }
            _ = ticker.tick() => {}
            () = run.wake.notified() => {}
        }

        // Looked at before the store's failure, which is recorded before
        // the chunk that met it stops being in flight: so a failure that
        // ends the transfer is never taken for a want of suppliers.
        let spent = tasks.is_empty()
            && discovered.is_empty()
            && slots.values().all(|slot| slot.dropped)
            && !run.claims.any_in_flight();
        if let Some(failure) = run.failure() {
            return Err(failure.into());
        }
        if spent && !run.is_complete() {
            return Err(Error::Unsupplied(last_dropped));
        }
        if run.idle() > run.options.stall_timeout {
            return Err(Error::Stalled(run.options.stall_timeout));
        }
    }

    tasks.shutdown().await;
    // A chunk can be marked done by one supplier while the store fails on
    // another's, so every chunk being done is not yet success.
    if let Some(failure) = run.failure() {
        return Err(failure.into());
    }
    Ok(Summary {
        sources: run.stored_from(),
        usable: slots
            .into_iter()
            .filter(|(_, slot)| !slot.dropped)
            .map(|(id, _)| id)
            .collect(),
        verification: run.store.verification(),
    })
}

/// What the manager remembers about one supplier.
struct Slot {
    supplier: Arc<dyn Supplier>,
    active: bool,
    dropped: bool,
    /// How many times in a row it has failed without delivering anything.
    failures: u32,
    retry_at: Instant,
    /// The bytes it had delivered when it was last started.
    stored_before: u64,
}

impl Slot {
    fn new(supplier: Arc<dyn Supplier>) -> Self {
        Self {
            supplier,
            active: false,
            dropped: false,
            failures: 0,
            retry_at: Instant::now(),
            stored_before: 0,
        }
    }

    /// Whether it is waiting to be started and may be.
    fn is_due(&self, now: Instant) -> bool {
        !self.active && !self.dropped && self.retry_at <= now
    }
}

/// State shared by every task working on one transfer.
struct Run {
    store: Arc<Store>,
    me: SupplierId,
    options: Options,
    observer: Observer,
    /// Woken when a chunk is released or the transfer completes.
    wake: Arc<Notify>,
    claims: Shared,
    /// When the transfer started: what its times are counted from.
    started: Instant,
    /// How many pipelines are taking runs of chunks, each with a request
    /// of its own.
    streams: AtomicU64,
    /// The number the next supplier fetches under.
    owners: AtomicU32,
    found: mpsc::UnboundedSender<Arc<dyn Supplier>>,
    stored_from: Mutex<HashMap<SupplierId, u64>>,
    /// When a chunk was last stored or a request last brought
    /// [`PROGRESS`] bytes, or when the transfer started.
    last_progress: Mutex<Instant>,
    /// Whether a supplier is waiting for a turn that none is free for.
    crowded: AtomicBool,
    /// The failure of the store that ends the transfer, once there is one.
    /// It is kept here because whoever was waiting for the chunk to be
    /// stored may have been stopped before it could pass the failure on.
    failed: Mutex<Option<Failure>>,
}

impl Run {
    fn new(
        store: Arc<Store>,
        me: SupplierId,
        options: Options,
        observer: Observer,
        found: mpsc::UnboundedSender<Arc<dyn Supplier>>,
    ) -> Arc<Self> {
        let wake = Arc::new(Notify::new());
        let claims = Claims::new(&store.held(), {
            let wake = wake.clone();
            move || wake.notify_waiters()
        });
        Arc::new(Self {
            store,
            me,
            options,
            observer,
            wake,
            claims,
            started: Instant::now(),
            streams: AtomicU64::new(0),
            owners: AtomicU32::new(0),
            found,
            stored_from: Mutex::new(HashMap::new()),
            last_progress: Mutex::new(Instant::now()),
            crowded: AtomicBool::new(false),
            failed: Mutex::new(None),
        })
    }

    /// Starts every supplier that is due, as far as there are turns free.
    fn start_due(
        self: &Arc<Self>,
        slots: &mut HashMap<SupplierId, Slot>,
        tasks: &mut JoinSet<Ended>,
        running: &mut HashMap<tokio::task::Id, SupplierId>,
    ) {
        let now = Instant::now();
        let free = self
            .options
            .max_suppliers
            .max(1)
            .saturating_sub(running.len());
        // Whoever has waited longest goes first, so that nobody waits for
        // ever behind suppliers that keep coming back.
        let mut due: Vec<(Instant, SupplierId)> = slots
            .iter()
            .filter(|(_, slot)| slot.is_due(now))
            .map(|(&id, slot)| (slot.retry_at, id))
            .collect();
        due.sort_unstable();
        // A supplier in use that has nothing wanted gives up its turn when
        // another is waiting for one.
        self.crowded.store(due.len() > free, Ordering::Relaxed);
        for (_, id) in due.into_iter().take(free) {
            let Some(slot) = slots.get_mut(&id) else {
                continue;
            };
            slot.active = true;
            slot.stored_before = self.stored_by(&id);
            let handle = tasks.spawn(supply(self.clone(), slot.supplier.clone()));
            running.insert(handle.id(), id);
        }
    }

    /// Records that the store failed, which ends the transfer.
    fn fail(&self, failure: Failure) {
        sync::lock(&self.failed).get_or_insert(failure);
        self.wake.notify_waiters();
    }

    /// The failure of the store, if it has failed.
    fn failure(&self) -> Option<Failure> {
        sync::lock(&self.failed).clone()
    }

    fn is_complete(&self) -> bool {
        self.claims.is_complete()
    }

    /// Records that there was progress: a chunk was stored, or enough of
    /// one arrived.
    fn progressed(&self) {
        *sync::lock(&self.last_progress) = Instant::now();
    }

    /// How long it has been since there was progress.
    fn idle(&self) -> Duration {
        sync::lock(&self.last_progress).elapsed()
    }

    /// A number for one supplier to fetch under, so that the schedule knows
    /// whose each chunk in flight is.
    fn new_owner(&self) -> Owner {
        self.owners.fetch_add(1, Ordering::Relaxed)
    }

    /// The time, in microseconds since the transfer started.
    fn now(&self) -> u64 {
        micros(self.started.elapsed())
    }

    /// Reserves for `owner` a run of missing chunks that the supplier
    /// holds, looking from `cursor` on.
    fn reserve(
        &self,
        owner: Owner,
        held: &Bitfield,
        cursor: &mut u64,
        size: Size,
    ) -> Option<Reserved> {
        Claims::reserve(&self.claims, owner, held, cursor, size, self.now())
    }

    /// Records a chunk that the store has verified and written.
    fn complete(&self, claim: Claim, from: SupplierId, bytes: u64, put: Put) {
        (self.observer)(Event::Stored {
            supplier: from,
            bytes,
        });
        if let Put::Completed(file) = put {
            (self.observer)(Event::Committed { file });
        }
        *sync::lock(&self.stored_from).entry(from).or_default() += bytes;
        self.progressed();
        self.claims.complete(claim);
    }

    /// Reports a supplier learned of from another supplier.
    fn discover(&self, supplier: Arc<dyn Supplier>) {
        let _ = self.found.send(supplier);
    }

    /// The bytes stored from one supplier so far.
    fn stored_by(&self, supplier: &SupplierId) -> u64 {
        sync::lock(&self.stored_from)
            .get(supplier)
            .copied()
            .unwrap_or(0)
    }

    /// Whether the supplier fetching as `owner`, which holds `held`, should
    /// make way: it is fetching nothing and has nothing that is wanted now,
    /// and another supplier is waiting.
    fn should_yield(&self, owner: Owner, held: &Bitfield) -> bool {
        self.crowded.load(Ordering::Relaxed) && !self.claims.has_work_for(owner, held)
    }

    fn stored_from(&self) -> Vec<(SupplierId, u64)> {
        let mut sources: Vec<_> = sync::lock(&self.stored_from)
            .iter()
            .map(|(&supplier, &bytes)| (supplier, bytes))
            .collect();
        sources.sort_by_key(|source| std::cmp::Reverse(source.1));
        sources
    }
}

/// Why work with a supplier ended.
enum Ended {
    /// Nothing left to do: the transfer is complete.
    Finished,
    /// The supplier holds nothing that is wanted now, and another is
    /// waiting for its turn.
    Idle,
    /// The supplier was unreachable or broke off. Worth trying again.
    Failed(String),
    /// The supplier sent something wrong, or cannot be used at all. It is
    /// never tried again.
    Dropped(String),
    /// The store failed, or what was being read changed. Either ends the
    /// whole transfer.
    Fatal(Failure),
}

impl Ended {
    /// What a supplier's own failure means for the supplier.
    fn by(failure: Failure) -> Self {
        match failure {
            Failure::Unreachable(why) | Failure::Absent(why) => Self::Failed(why),
            Failure::Corrupt(why) | Failure::Fatal(why) => Self::Dropped(why),
            // No other supplier can make up for this: what has been stored
            // may be of two versions.
            changed @ Failure::Changed(_) => Self::Fatal(changed),
        }
    }
}

/// What one supplier is known to hold, for as long as it stays open.
struct View {
    /// The chunks it last said it holds, less those it then did not have.
    held: Bitfield,
    /// Chunks it said it held and then did not supply. Whatever it says
    /// later, it is not asked for them again: a supplier that keeps
    /// offering what it will not give would otherwise keep its turn for
    /// ever.
    refused: Vec<u64>,
}

impl View {
    fn new(chunks: u64) -> Self {
        Self {
            held: Bitfield::new(chunks),
            refused: Vec::new(),
        }
    }

    /// Takes what the supplier now says it holds.
    fn learn(&mut self, held: Bitfield) {
        self.held = held;
        for &index in &self.refused {
            self.held.clear(index);
        }
    }

    /// Records that the supplier did not have a chunk it said it held.
    fn refuse(&mut self, index: u64) {
        self.held.clear(index);
        self.refused.push(index);
    }
}

/// Everything a transfer does with one supplier.
async fn supply(run: Arc<Run>, supplier: Arc<dyn Supplier>) -> Ended {
    let id = supplier.id();
    let supply = match timeout(OPEN_TIMEOUT, supplier.open()).await {
        Ok(Ok(supply)) => supply,
        Ok(Err(failure)) => return Ended::by(failure),
        Err(_) => return Ended::Failed("it could not be reached in time".into()),
    };
    let _open = Open::new(&run, id);

    let chunks = run.store.tree().chunk_count();
    let view = Arc::new(RwLock::new(View::new(chunks)));
    if let Err(ended) = refresh(&run, &*supply, &view).await {
        return ended;
    }

    let mut workers = JoinSet::new();
    let owner = run.new_owner();
    let capabilities = supply.capabilities();
    // Every pipeline looks for its next chunks from one place, so that
    // between them they take the chunks in a row. One stream is read from
    // its start.
    let from = if capabilities.random_access {
        starting_point(&run.me, &id, chunks)
    } else {
        0
    };
    let cursor = Cursor::new(Mutex::new(from));
    // A supplier that takes every request on one connection is asked for
    // a chunk with each, by as many pipelines as its route has room for.
    let paced = (capabilities.random_access && capabilities.multiplexed).then(|| {
        let most = pace::most(u64::from(run.store.tree().chunk_size()));
        Arc::new(Paced::new(most))
    });
    // How many of this supplier's pipelines make requests of their own.
    let fellows = Arc::new(AtomicU64::new(0));
    let pipeline = |paced: Option<Arc<Paced>>| Pipeline {
        run: run.clone(),
        supply: supply.clone(),
        from: id,
        view: view.clone(),
        owner,
        cursor: cursor.clone(),
        // Counted here and not when the pipeline starts work, so that the
        // first to reserve a run shares the work with all of them.
        stream: paced.is_none().then(|| Stream::begin(&run, &fellows)),
        paced,
        in_order: !capabilities.random_access,
    };
    // Any other makes a set number of requests, each for a run of chunks:
    // one, if it gives its files only from their start.
    let requests = match (&paced, capabilities.random_access) {
        (Some(_), _) => 0,
        (None, true) => run.options.requests_per_supplier.max(1) as u64,
        (None, false) => 1,
    };
    let streams: Vec<Pipeline> = (0..requests).map(|_| pipeline(None)).collect();
    for stream in streams {
        workers.spawn(work(stream));
    }

    let mut ticker = tokio::time::interval(STATUS_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    ticker.tick().await;
    loop {
        if let Some(paced) = &paced {
            for _ in 0..paced.to_start() {
                workers.spawn(work(pipeline(Some(paced.clone()))));
            }
        }
        tokio::select! {
            joined = workers.join_next() => match joined {
                None => return Ended::Finished,
                Some(Ok(Ok(()))) => {}
                // Dropping the set cancels the other workers, which releases
                // whatever chunks they had reserved.
                Some(Ok(Err(ended))) => return ended,
                Some(Err(e)) => return Ended::Failed(format!("a worker crashed: {e}")),
            },
            _ = ticker.tick() => {
                if let Err(ended) = refresh(&run, &*supply, &view).await {
                    return ended;
                }
                if run.should_yield(owner, &sync::read(&view).held) {
                    return Ended::Idle;
                }
            }
            () = Paced::wants_more(paced.as_deref()) => {}
        }
    }
}

/// Where the pipelines of one supplier look for their next chunks. Sharing
/// it, they take the chunks in a row between them.
type Cursor = Arc<Mutex<u64>>;

/// A pipeline that makes requests of its own, counted for as long as this
/// lives: among all such pipelines of the transfer, which is what work is
/// shared out between, and among those of its supplier.
struct Stream {
    run: Arc<Run>,
    fellows: Arc<AtomicU64>,
    /// Whether it has stepped aside, and is counted among its fellows no
    /// more.
    aside: bool,
}

impl Stream {
    fn begin(run: &Arc<Run>, fellows: &Arc<AtomicU64>) -> Self {
        run.streams.fetch_add(1, Ordering::Relaxed);
        fellows.fetch_add(1, Ordering::Relaxed);
        Self {
            run: run.clone(),
            fellows: fellows.clone(),
            aside: false,
        }
    }

    /// Steps aside if another pipeline of the supplier carries on, and
    /// says whether it did. Of several that ask at the same moment, one is
    /// always left: the last of a supplier does not step aside, and what
    /// stopped it is then the supplier's failure.
    fn steps_aside(&mut self) -> bool {
        let mut fellows = self.fellows.load(Ordering::Relaxed);
        while fellows > 1 && !self.aside {
            let fewer = self.fellows.compare_exchange_weak(
                fellows,
                fellows - 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            );
            match fewer {
                Ok(_) => self.aside = true,
                Err(now) => fellows = now,
            }
        }
        self.aside
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.run.streams.fetch_sub(1, Ordering::Relaxed);
        if !self.aside {
            self.fellows.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

/// The pipelines of one supplier that takes many requests at once: how many
/// there are, and how many there are to be.
struct Paced {
    /// The most there may be.
    most: u64,
    counts: Mutex<Counts>,
    /// Told when there are to be more pipelines than there are.
    more: Notify,
}

struct Counts {
    /// How many pipelines there are to be.
    wanted: u64,
    /// How many there are.
    running: u64,
}

impl Paced {
    /// One pipeline to begin with.
    fn new(most: u64) -> Self {
        Self {
            most,
            counts: Mutex::new(Counts {
                wanted: 1,
                running: 0,
            }),
            more: Notify::new(),
        }
    }

    /// Takes in how long a chunk took to come.
    fn learn(&self, taken: Duration) {
        let mut counts = sync::lock(&self.counts);
        counts.wanted = pace::next(counts.wanted, millis(taken), self.most);
        if counts.wanted > counts.running {
            self.more.notify_one();
        }
    }

    /// How many pipelines to start now. They are counted as started.
    fn to_start(&self) -> u64 {
        let mut counts = sync::lock(&self.counts);
        let short = counts.wanted.saturating_sub(counts.running);
        counts.running += short;
        short
    }

    /// Whether the pipeline that asks is one too many. One that is, is
    /// counted out, and stops.
    fn is_over(&self) -> bool {
        let mut counts = sync::lock(&self.counts);
        let over = counts.running > counts.wanted;
        if over {
            counts.running -= 1;
        }
        over
    }

    /// Waits until `paced` is to have more pipelines than it has. For ever,
    /// if there is nothing paced.
    async fn wants_more(paced: Option<&Self>) {
        match paced {
            Some(paced) => paced.more.notified().await,
            None => std::future::pending().await,
        }
    }
}

/// Asks the supplier what it holds and whom it knows.
async fn refresh(run: &Run, supply: &dyn Supply, view: &RwLock<View>) -> Result<(), Ended> {
    let status = match timeout(STATUS_TIMEOUT, supply.status()).await {
        Ok(Ok(status)) => status,
        Ok(Err(failure)) => return Err(Ended::by(failure)),
        Err(_) => return Err(Ended::Failed("it did not say what it holds in time".into())),
    };
    if !status.held.is_well_formed(run.store.tree().chunk_count()) {
        return Err(Ended::Dropped(
            "it described chunks the artifact does not have".into(),
        ));
    }
    sync::write(view).learn(status.held);
    for other in status.others.into_iter().take(MAX_OTHERS_PER_STATUS) {
        run.discover(other);
    }
    run.wake.notify_waiters();
    Ok(())
}

/// What one request pipeline works with.
struct Pipeline {
    run: Arc<Run>,
    supply: Arc<dyn Supply>,
    from: SupplierId,
    view: Arc<RwLock<View>>,
    owner: Owner,
    cursor: Cursor,
    /// Set if the supplier takes as many requests as its route has room
    /// for. Each is then for one chunk.
    paced: Option<Arc<Paced>>,
    /// Whether the supplier gives its files only from their start. Its one
    /// pipeline then takes every chunk it can in a row.
    in_order: bool,
    /// Set if the pipeline makes requests of its own, each for a run.
    stream: Option<Stream>,
}

/// One request pipeline: reserve a run of chunks, fetch them one after
/// another, and hand each to the store.
async fn work(pipeline: Pipeline) -> Result<(), Ended> {
    let Pipeline {
        run,
        supply,
        from,
        view,
        owner,
        cursor,
        paced,
        in_order,
        mut stream,
    } = pipeline;
    // How long a chunk took to come over the last run, in microseconds.
    let mut micros_each = 0;
    loop {
        if run.is_complete() {
            return Ok(());
        }
        let size = match (&paced, in_order) {
            (Some(_), _) => Size::One,
            (None, true) => Size::Run {
                streams: 1,
                micros_each,
            },
            (None, false) => Size::Run {
                streams: run.streams.load(Ordering::Relaxed),
                micros_each,
            },
        };
        let reserved = {
            let mut cursor = sync::lock(&cursor);
            run.reserve(owner, &sync::read(&view).held, &mut cursor, size)
        };
        let Some(mut reserved) = reserved else {
            // Nothing is free that the supplier holds. Another pipeline
            // may have more still to ask for than it will soon get to:
            // then what this one could fetch of it in that time is put
            // back, to be had from here.
            let spared = {
                let held = &sync::read(&view).held;
                run.claims.spare(held, run.now(), micros_each)
            };
            if spared {
                continue;
            }
            // Or the supplier may gain chunks, or another may release one
            // it holds.
            tokio::select! {
                () = run.wake.notified() => {}
                () = tokio::time::sleep(IDLE_POLL) => {}
            }
            continue;
        };

        let (began, mut fetched) = (Instant::now(), 0u64);
        loop {
            let (claim, following) = match reserved.next() {
                Next::Chunk(claim, following) => (claim, following),
                Next::Over => break,
                Next::Cut(at) => {
                    // What the supplier kept open to be read on from is
                    // not going to be.
                    supply.leave(at);
                    break;
                }
            };
            let index = claim.index;

            let asked = Instant::now();
            let data = match fetch(&run, &*supply, index, following).await {
                Some(Ok(data)) => data,
                Some(Err(Failure::Absent(_))) => {
                    sync::write(&view).refuse(index);
                    continue;
                }
                // A request that the supplier would not take, or that it
                // has let starve, while its other requests carry on: it
                // takes fewer requests at once than it was given, and is
                // used with those it takes. What this one had reserved
                // goes back to the others.
                Some(Err(Failure::Unreachable(why)))
                    if fetched == 0 && stream.as_mut().is_some_and(Stream::steps_aside) =>
                {
                    debug!(supplier = %short(&from), "one request fewer at a time: {why}");
                    return Ok(());
                }
                None if stream.as_mut().is_some_and(Stream::steps_aside) => {
                    debug!(supplier = %short(&from), "one request fewer at a time: chunk {index} did not arrive in time");
                    return Ok(());
                }
                Some(Err(failure)) => return Err(Ended::by(failure)),
                None => {
                    return Err(Ended::Failed(format!(
                        "chunk {index} did not arrive in time"
                    )));
                }
            };
            let taken = asked.elapsed();
            if let Some(paced) = &paced {
                paced.learn(taken);
            }
            // How fast the run is going is taken over the whole of it. A
            // link gives its bytes in bursts, and one chunk says little:
            // judged by its last chunk, the stream that happens to finish
            // first looks faster than it is and takes too much.
            fetched += 1;
            micros_each = (micros(began.elapsed()) / fetched).max(1);
            reserved.learn(micros_each, run.now());

            see_through(&run, claim, from, data).await?;
            if paced.as_ref().is_some_and(|paced| paced.is_over()) {
                return Ok(());
            }
        }
    }
}

/// Has a chunk that arrived stored, and waits until it is. From here the
/// chunk is seen through, whatever becomes of its supplier: it is stored,
/// counted and marked done as one step.
async fn see_through(
    run: &Arc<Run>,
    claim: Claim,
    from: SupplierId,
    data: Vec<u8>,
) -> Result<(), Ended> {
    let index = claim.index;
    match tokio::spawn(keep(run.clone(), claim, from, data)).await {
        Ok(Ok(Put::Refused)) => Err(Ended::Dropped(format!("chunk {index} failed verification"))),
        Ok(Ok(_)) => Ok(()),
        Ok(Err(failure)) => Err(Ended::Fatal(failure)),
        Err(e) => Err(Ended::Failed(format!(
            "storing chunk {index} was cut short: {e}"
        ))),
    }
}

/// A length of time in whole milliseconds.
fn millis(time: Duration) -> u64 {
    u64::try_from(time.as_millis()).unwrap_or(u64::MAX)
}

/// A length of time in whole microseconds.
fn micros(time: Duration) -> u64 {
    u64::try_from(time.as_micros()).unwrap_or(u64::MAX)
}

/// Fetches chunk `index`, after which `following` more are to be asked
/// for, for as long as it keeps coming. `None` if next to nothing of it
/// arrived for [`CHUNK_TIMEOUT`]: a chunk that is slow is waited for, and
/// one that is not coming is not.
async fn fetch(
    run: &Run,
    supply: &dyn Supply,
    index: u64,
    following: u64,
) -> Option<Result<Vec<u8>, Failure>> {
    let arrived = Arrived::default();
    let mut chunk = supply.chunk(index, following, &arrived);
    let mut watch = tokio::time::interval_at(Instant::now() + WATCH, WATCH);
    watch.set_missed_tick_behavior(MissedTickBehavior::Delay);
    // What had arrived when the request last made progress, and when.
    let (mut marked, mut since) = (0, Instant::now());
    loop {
        tokio::select! {
            fetched = &mut chunk => return Some(fetched),
            _ = watch.tick() => {
                let now = arrived.so_far();
                if now - marked >= PROGRESS {
                    (marked, since) = (now, Instant::now());
                    run.progressed();
                } else if since.elapsed() > CHUNK_TIMEOUT {
                    return None;
                }
            }
        }
    }
}

/// Hands a fetched chunk to the store, which checks it against the
/// manifest before keeping it, and records it as done if it was kept.
async fn keep(
    run: Arc<Run>,
    claim: Claim,
    from: SupplierId,
    data: Vec<u8>,
) -> Result<Put, Failure> {
    let bytes = data.len() as u64;
    let put = match run.store.write_chunk(claim.index, data).await {
        Ok(put) => put,
        Err(failure) => {
            run.fail(failure.clone());
            return Err(failure);
        }
    };
    if put != Put::Refused {
        run.complete(claim, from, bytes, put);
    }
    Ok(put)
}

/// Where the pipelines of a supplier start looking for chunks: a place on
/// the ring of chunks that differs for each pair of ends. Ends that start
/// at the same moment therefore collect different chunks first and can
/// trade them afterwards.
fn starting_point(me: &SupplierId, supplier: &SupplierId, chunks: u64) -> u64 {
    stagger(me, supplier) % chunks.max(1)
}

/// A starting offset that differs for each pair of ends.
fn stagger(me: &SupplierId, supplier: &SupplierId) -> u64 {
    let mut mixed = [0u8; 8];
    for (i, byte) in mixed.iter_mut().enumerate() {
        *byte = me[i] ^ supplier[i + 8];
    }
    u64::from_le_bytes(mixed)
}

/// First eight hex digits of a supplier's id, for log lines.
fn short(id: &SupplierId) -> String {
    model::hex::encode(&id[..4])
}

/// Reports a supplier as in use for as long as it lives.
struct Open<'a> {
    run: &'a Run,
    supplier: SupplierId,
}

impl<'a> Open<'a> {
    fn new(run: &'a Run, supplier: SupplierId) -> Self {
        (run.observer)(Event::Opened { supplier });
        Self { run, supplier }
    }
}

impl Drop for Open<'_> {
    fn drop(&mut self) {
        (self.run.observer)(Event::Closed {
            supplier: self.supplier,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An id whose bytes count up from `first`.
    fn id(first: u8) -> SupplierId {
        std::array::from_fn(|i| first + u8::try_from(i).unwrap())
    }

    #[test]
    fn the_stagger_mixes_the_start_of_one_id_with_the_middle_of_the_other() {
        // Bits chosen so that mixing them any other way gives another answer.
        assert_eq!(
            stagger(&[0b1100; 32], &[0b1010; 32]),
            u64::from_le_bytes([0b0110; 8])
        );
        let expected: [u8; 8] = std::array::from_fn(|i| {
            let i = u8::try_from(i).unwrap();
            i ^ (100 + i + 8)
        });
        assert_eq!(stagger(&id(0), &id(100)), u64::from_le_bytes(expected));
        assert_ne!(stagger(&id(0), &id(100)), stagger(&id(1), &id(100)));
        assert_ne!(stagger(&id(0), &id(100)), stagger(&id(0), &id(101)));
    }

    #[test]
    fn a_copy_starts_at_the_first_chunk() {
        use crate::copy::{HERE, source_id};
        for number in [0, 1, 300] {
            let source = source_id(number);
            assert_ne!(source, HERE);
            assert_ne!(source, source_id(number + 1));
            assert_eq!(starting_point(&HERE, &source, 8), 0);
        }
    }

    #[test]
    fn a_starting_point_is_always_a_chunk() {
        let (me, supplier) = (id(0), id(100));
        assert_eq!(
            starting_point(&me, &supplier, 10),
            stagger(&me, &supplier) % 10
        );
        // The last pair of ends has the largest stagger there is.
        for (me, supplier) in [(id(7), id(9)), ([0xff; 32], [0; 32])] {
            for chunks in [0, 1, 3, u64::MAX / 8] {
                assert!(starting_point(&me, &supplier, chunks) < chunks.max(1));
            }
        }
    }

    #[test]
    fn a_supplier_is_named_by_the_start_of_its_id() {
        assert_eq!(short(&[0xab; 32]), "abababab");
    }
}
