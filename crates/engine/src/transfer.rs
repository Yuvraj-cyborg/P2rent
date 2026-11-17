//! Fetching what a store lacks from whoever can supply it.
//!
//! A transfer pulls the missing chunks from as many suppliers as it can
//! find. Every chunk is checked against the manifest before it is stored,
//! and a file gets its final name only when every one of its chunks is in
//! place. What a supplier is, a machine or a web server or a folder, is
//! nothing to this module.

use crate::claims::{Claim, Claims, Shared};
use crate::store::{Put, Store};
use crate::sync;
use model::artifact::Bitfield;
use model::place::{Event, Failure, Observer, Supplier, SupplierId, Supply, Verification};
use model::retry;
use model::schedule::Owner;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use tokio::sync::{Notify, mpsc};
use tokio::task::JoinSet;
use tokio::time::{Instant, MissedTickBehavior, timeout};
use tracing::{debug, warn};

const OPEN_TIMEOUT: Duration = Duration::from_secs(10);
const STATUS_TIMEOUT: Duration = Duration::from_secs(10);
const CHUNK_TIMEOUT: Duration = Duration::from_secs(60);
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
    /// Chunks requested in parallel from each supplier.
    pub requests_per_supplier: usize,
    /// Give up when no chunk has arrived for this long.
    pub stall_timeout: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            max_suppliers: 8,
            requests_per_supplier: 4,
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

        if let Some(failure) = run.failure() {
            return Err(failure.into());
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
    /// The number the next supplier fetches under.
    owners: AtomicU32,
    found: mpsc::UnboundedSender<Arc<dyn Supplier>>,
    stored_from: Mutex<HashMap<SupplierId, u64>>,
    /// When a chunk was last stored, or when the transfer started.
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

    /// How long it has been since a chunk was stored.
    fn idle(&self) -> Duration {
        sync::lock(&self.last_progress).elapsed()
    }

    /// A number for one supplier to fetch under, so that the schedule knows
    /// whose each chunk in flight is.
    fn new_owner(&self) -> Owner {
        self.owners.fetch_add(1, Ordering::Relaxed)
    }

    /// Reserves for `owner` a missing chunk that the supplier holds,
    /// scanning from `cursor`.
    fn claim(&self, owner: Owner, held: &Bitfield, cursor: &mut u64) -> Option<Claim> {
        Claims::claim(&self.claims, owner, held, cursor)
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
        *sync::lock(&self.last_progress) = Instant::now();
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
    let cursors: Vec<u64> = if supply.capabilities().random_access {
        let requests = run.options.requests_per_supplier.max(1) as u64;
        starting_points(&run.me, &id, chunks, requests).collect()
    } else {
        // One stream is read by one pipeline, from its start.
        vec![0]
    };
    for cursor in cursors {
        workers.spawn(work(
            run.clone(),
            supply.clone(),
            id,
            view.clone(),
            owner,
            cursor,
        ));
    }

    let mut ticker = tokio::time::interval(STATUS_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    ticker.tick().await;
    loop {
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

/// One request pipeline: claim a chunk, fetch it, and hand it to the store.
async fn work(
    run: Arc<Run>,
    supply: Arc<dyn Supply>,
    from: SupplierId,
    view: Arc<RwLock<View>>,
    owner: Owner,
    mut cursor: u64,
) -> Result<(), Ended> {
    loop {
        if run.is_complete() {
            return Ok(());
        }
        let claim = run.claim(owner, &sync::read(&view).held, &mut cursor);
        let Some(claim) = claim else {
            // The supplier has nothing we need right now. It may gain
            // chunks, or another supplier may release one it holds.
            tokio::select! {
                () = run.wake.notified() => {}
                () = tokio::time::sleep(IDLE_POLL) => {}
            }
            continue;
        };
        let index = claim.index;

        let data = match timeout(CHUNK_TIMEOUT, supply.chunk(index)).await {
            Ok(Ok(data)) => data,
            Ok(Err(Failure::Absent(_))) => {
                sync::write(&view).refuse(index);
                continue;
            }
            Ok(Err(failure)) => return Err(Ended::by(failure)),
            Err(_) => {
                return Err(Ended::Failed(format!(
                    "chunk {index} did not arrive in time"
                )));
            }
        };

        // From here the chunk is seen through, whatever becomes of this
        // supplier: it is stored, counted and marked done as one step.
        match tokio::spawn(keep(run.clone(), claim, from, data)).await {
            Ok(Ok(Put::Refused)) => {
                return Err(Ended::Dropped(format!("chunk {index} failed verification")));
            }
            Ok(Ok(_)) => {}
            Ok(Err(failure)) => return Err(Ended::Fatal(failure)),
            Err(e) => {
                return Err(Ended::Failed(format!(
                    "storing chunk {index} was cut short: {e}"
                )));
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

/// Where each of a supplier's `requests` pipelines starts looking for a
/// chunk: spread evenly round the ring of chunks, from a place that differs
/// for each pair of ends. Ends that start at the same moment therefore
/// collect different chunks first and can trade them afterwards.
fn starting_points(
    me: &SupplierId,
    supplier: &SupplierId,
    chunks: u64,
    requests: u64,
) -> impl Iterator<Item = u64> {
    let total = chunks.max(1);
    // Reduced first, so that adding a pipeline's share cannot overflow.
    let origin = stagger(me, supplier) % total;
    (0..requests).map(move |pipeline| (origin + pipeline * total / requests) % total)
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
    fn pipelines_start_evenly_spread_round_the_chunks() {
        let (me, supplier) = (id(0), id(100));
        let origin = stagger(&me, &supplier) % 8;
        let points: Vec<u64> = starting_points(&me, &supplier, 8, 4).collect();
        let spread = [0, 2, 4, 6].map(|step| (origin + step) % 8);
        assert_eq!(points, spread);

        // Three pipelines over ten chunks: as even as whole numbers allow.
        let origin = stagger(&me, &supplier) % 10;
        let points: Vec<u64> = starting_points(&me, &supplier, 10, 3).collect();
        assert_eq!(points, [0, 3, 6].map(|step| (origin + step) % 10));
    }

    #[test]
    fn a_starting_point_is_always_a_chunk() {
        // The last pair of ends has the largest stagger there is, which
        // must not overflow on the way to a starting point.
        for (me, supplier) in [(id(7), id(9)), ([0xff; 32], [0; 32])] {
            for (chunks, requests) in [(0, 4), (1, 4), (3, 8), (u64::MAX / 8, 8)] {
                let points: Vec<u64> = starting_points(&me, &supplier, chunks, requests).collect();
                assert_eq!(points.len() as u64, requests);
                assert!(points.iter().all(|&point| point < chunks.max(1)));
            }
        }
    }

    #[test]
    fn a_supplier_is_named_by_the_start_of_its_id() {
        assert_eq!(short(&[0xab; 32]), "abababab");
    }
}
