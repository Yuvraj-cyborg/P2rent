//! Places data is read from and written to.
//!
//! A backend is a thin adapter between one kind of place and these two
//! traits. It moves bytes. Deciding which bytes to move, checking them and
//! deciding when a file is whole are done once, by the engine, for every
//! place.
//!
//! Reading is asynchronous: bytes are fetched from many sources at once,
//! and a fetch that is no longer wanted is dropped. Writing is not: a write
//! that has begun is always finished, so it is a plain call.
//!
//! A place is opened for one tree: the entries of one artifact. A file is
//! named by its position among those entries.

use crate::artifact::{Bitfield, Entry, EntryMetadata, FileDigest, Hash, Tree};
use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;
use core::future::Future;
use core::pin::Pin;
use thiserror::Error;

/// How an operation on a place failed, by what can be done about it.
///
/// The text is for a person. The kind is for the engine, which is why a
/// backend sorts its own errors into these five and the engine never has to
/// know what a socket or a disk is.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum Failure {
    /// The place could not be reached, or the connection broke. Trying
    /// again later may work.
    #[error("{0}")]
    Unreachable(String),

    /// The place does not hold what was asked for.
    #[error("{0}")]
    Absent(String),

    /// The place answered with something it must not. It is not used again.
    #[error("{0}")]
    Corrupt(String),

    /// What is being read is no longer what it was when the transfer
    /// began. No part of it can be trusted to go with the rest, so the
    /// transfer ends and what it has written so far is thrown away.
    #[error("{0}")]
    Changed(String),

    /// Trying again gains nothing: the disk is full, or access is refused.
    #[error("{0}")]
    Fatal(String),
}

/// A result that is still on its way. Reading returns one, so that many
/// reads can be waited for at once and any of them given up.
pub type Pending<'a, T> = Pin<Box<dyn Future<Output = Result<T, Failure>> + Send + 'a>>;

/// Gives the digest a chunk must have for a place to count it as held, or
/// `None` when no digest is known for that chunk, which is then not held.
pub type Expected<'a> = dyn Fn(u64) -> Option<Hash> + Sync + 'a;

/// What a place can do, so that the engine knows how it may be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// Any range of a file can be reached, in any order. Without this a
    /// file is one stream from its first byte to its last.
    pub random_access: bool,
    /// Every request travels on one connection, so one more request costs
    /// nothing but its bytes. The engine then keeps as many in flight as
    /// the route has room for ([`crate::pace`]), where otherwise it keeps
    /// to a set number. It means nothing without `random_access`.
    pub multiplexed: bool,
}

/// What is known about a file before any of it has been read, when the
/// digests of its chunks are not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Foreknown {
    /// What the source gives to tell one version of the file from another:
    /// bytes that change when the file does. Empty if it gives nothing, and
    /// then a transfer that is interrupted starts the file again.
    pub validator: Vec<u8>,
    /// The digest of the whole file, if the source or the user gave one.
    /// The file is committed only if its bytes have it.
    pub digest: Option<FileDigest>,
}

/// What a source holds, as far as it can say without being read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Description {
    /// The name of the root.
    pub name: String,
    /// Every entry with what is recorded about it, in order, the root
    /// first.
    pub entries: Vec<(Entry, EntryMetadata)>,
    /// What is known beforehand of each entry.
    pub known: Vec<Foreknown>,
}

impl Description {
    /// What a place that holds `tree` says of it, when it knows nothing
    /// beforehand about any file.
    pub fn of(tree: &Tree) -> Self {
        let entries = tree.entries().iter().enumerate();
        Self {
            name: tree.name().into(),
            entries: entries
                .filter_map(|(entry, described)| Some((described.clone(), tree.metadata(entry)?)))
                .collect(),
            known: tree
                .entries()
                .iter()
                .map(|_| Foreknown::default())
                .collect(),
        }
    }
}

/// A place data can be read from.
pub trait Source: fmt::Debug + Send + Sync {
    /// What reading from this place allows.
    fn capabilities(&self) -> Capabilities;

    /// Says what is here.
    fn describe(&self) -> Pending<'_, Description>;

    /// Opens `file` to be read in order, starting at `offset`.
    ///
    /// This is how many chunks are fetched: one after another from one
    /// reader, which for a web server is one request and not one for each
    /// chunk. A place without [`Capabilities::random_access`] may have to
    /// read and discard everything before `offset` to get there.
    fn open(&self, file: usize, offset: u64) -> Pending<'_, Box<dyn Reader>>;

    /// Reads `len` bytes of `file`, starting at `offset`.
    ///
    /// The whole range is returned or the read fails: a place never
    /// answers with fewer bytes than were asked for.
    fn read(&self, file: usize, offset: u64, len: u32) -> Pending<'_, Vec<u8>> {
        Box::pin(async move { self.open(file, offset).await?.read(len).await })
    }
}

/// The bytes of one file, read in order from where it was opened.
pub trait Reader: fmt::Debug + Send {
    /// Reads the next `len` bytes: all of them, or the read fails.
    fn read(&mut self, len: u32) -> Pending<'_, Vec<u8>>;
}

/// A place data can be written to.
///
/// A file being written is kept apart from the file under its final name
/// until it is committed, so the final name never holds a file with holes.
///
/// These calls do their work before they return, and may wait on a disk or
/// a network to do it. The engine makes each one on a thread that is
/// allowed to wait, and never stops one halfway: a file it has counted as
/// whole is always committed.
pub trait Sink: fmt::Debug + Send + Sync {
    /// What writing to this place allows.
    fn capabilities(&self) -> Capabilities;

    /// Makes the place ready for the tree, and says which chunks it already
    /// holds. A chunk is reported only if the place has checked that its
    /// bytes have the digest `expected` gives for it, so whatever an earlier
    /// transfer left behind is kept if it is right and fetched again if it
    /// is not. `examined` is called with the size of each chunk looked at.
    ///
    /// # Errors
    ///
    /// Returns a failure if the place cannot be made ready.
    fn begin(
        &self,
        expected: &Expected<'_>,
        examined: &(dyn Fn(u64) + Sync),
    ) -> Result<Bitfield, Failure>;

    /// Writes `data` into `file` at `offset`. Nothing of it is visible
    /// under the file's final name until [`Sink::commit`].
    ///
    /// # Errors
    ///
    /// Returns a failure if the bytes cannot be written.
    fn write(&self, file: usize, offset: u64, data: &[u8]) -> Result<(), Failure>;

    /// Reads back `len` bytes that were written to `file` at `offset` and
    /// are not committed yet. The engine does this to check a whole file
    /// before it commits it.
    ///
    /// # Errors
    ///
    /// Returns a failure if the range was not written or cannot be read.
    fn read_back(&self, file: usize, offset: u64, len: u32) -> Result<Vec<u8>, Failure>;

    /// Returns the notes kept beside what is being written to `file`: bytes
    /// that mean something only to the engine, which uses them to carry on
    /// with a transfer that was interrupted. Empty if there are none.
    ///
    /// No more than `limit` bytes are returned, from the start. Notes can
    /// be read before [`Sink::begin`]. They last until the file is
    /// committed or aborted.
    ///
    /// # Errors
    ///
    /// Returns a failure if there are notes and they cannot be read.
    fn notes(&self, file: usize, limit: usize) -> Result<Vec<u8>, Failure>;

    /// Writes `data` into the notes of `file` at `offset`. It is called
    /// after [`Sink::begin`].
    ///
    /// # Errors
    ///
    /// Returns a failure if the notes cannot be written.
    fn note(&self, file: usize, offset: u64, data: &[u8]) -> Result<(), Failure>;

    /// Gives `file` its final name, in one step. It is called once every
    /// byte of the file has been written.
    ///
    /// # Errors
    ///
    /// Returns a failure if the file cannot be given its name.
    fn commit(&self, file: usize) -> Result<(), Failure>;

    /// Throws away what has been written to `file` and not committed.
    /// Whatever has its final name is left as it is.
    ///
    /// # Errors
    ///
    /// Returns a failure if something is there and cannot be removed.
    fn abort(&self, file: usize) -> Result<(), Failure>;

    /// Says whether what is committed for `file` can be taken for the file
    /// without reading it: it has the size the tree gives, and was last
    /// changed at the time the tree records. A place that cannot tell, or
    /// a tree that records no time, says no.
    ///
    /// This is how a transfer with no digests leaves alone a file that is
    /// already there. It is weaker than a digest, and is used only where
    /// there is none.
    ///
    /// # Errors
    ///
    /// Returns a failure if the place cannot be looked at.
    fn is_current(&self, file: usize) -> Result<bool, Failure>;

    /// Finishes the tree once every file is committed, and makes what was
    /// committed last through a power cut.
    ///
    /// # Errors
    ///
    /// Returns a failure if the tree cannot be finished.
    fn finish(&self) -> Result<(), Failure>;
}

/// Tells one supplier from another within a transfer: a node id for a
/// machine, a digest of its address for a web server.
pub type SupplierId = [u8; 32];

/// Something that can supply the chunks of one artifact: another machine, a
/// web server, a folder.
///
/// The engine decides which chunk to ask each supplier for, checks what
/// comes back, and stops using a supplier that sends anything wrong. A
/// supplier fetches bytes and nothing more.
pub trait Supplier: fmt::Debug + Send + Sync {
    /// What tells this supplier from every other.
    fn id(&self) -> SupplierId;

    /// Reaches the supplier. Nothing is asked of it before this, and
    /// dropping what is returned lets go of it.
    fn open(&self) -> Pending<'_, Arc<dyn Supply>>;
}

/// A supplier that has been reached.
pub trait Supply: fmt::Debug + Send + Sync {
    /// Says which chunks the supplier holds now and which other suppliers
    /// it knows of. It is asked again from time to time, because a supplier
    /// that is itself still fetching gains chunks.
    fn status(&self) -> Pending<'_, Status>;

    /// Fetches chunk `index`. The failure is [`Failure::Absent`] if the
    /// supplier turns out not to hold it.
    fn chunk(&self, index: u64) -> Pending<'_, Vec<u8>>;

    /// What fetching from this supplier allows. One without
    /// [`Capabilities::random_access`] is asked for one chunk at a time,
    /// from the first that is missing onwards.
    fn capabilities(&self) -> Capabilities;
}

/// What a supplier says about itself.
#[derive(Debug)]
pub struct Status {
    /// The chunks it holds.
    pub held: Bitfield,
    /// Other suppliers of the same artifact that it knows of.
    pub others: Vec<Arc<dyn Supplier>>,
}

/// Something that happened in a transfer. The engine reports each one as it
/// happens, to whoever wants to show or record it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// What there is to copy is known: the sources have said what they
    /// hold.
    Planned {
        /// The bytes of every file.
        bytes: u64,
        /// The number of files.
        files: u64,
    },
    /// Bytes that were already at the destination have been looked at, to
    /// find out which of them can be kept.
    Examined {
        /// How many.
        bytes: u64,
    },
    /// What was already at the destination has been looked through, and
    /// this much of it is kept: it will not be fetched.
    Kept {
        /// How many bytes.
        bytes: u64,
    },
    /// A supplier was reached and is in use.
    Opened {
        /// The supplier.
        supplier: SupplierId,
    },
    /// A supplier is no longer in use.
    Closed {
        /// The supplier.
        supplier: SupplierId,
    },
    /// A chunk arrived, was checked and is stored.
    Stored {
        /// Where it came from.
        supplier: SupplierId,
        /// Its size.
        bytes: u64,
    },
    /// A file is whole and has its final name.
    Committed {
        /// Its position among the entries of the tree.
        file: usize,
    },
}

/// Called with each event of a transfer.
pub type Observer = Arc<dyn Fn(Event) + Send + Sync>;

/// How far the bytes of a finished transfer were checked. A transfer is
/// never reported as more than was checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verification {
    /// The digest of every chunk was known before the transfer began, and
    /// every chunk was compared with it before it was kept.
    Verified,
    /// No digest was known. The size and the server's validator stayed the
    /// same throughout, and the digest was computed and reported.
    Consistent,
}
