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

use crate::artifact::Bitfield;
use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;
use core::future::Future;
use core::pin::Pin;
use thiserror::Error;

/// How an operation on a place failed, by what can be done about it.
///
/// The text is for a person. The kind is for the engine, which is why a
/// backend sorts its own errors into these four and the engine never has to
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

    /// Trying again gains nothing: the disk is full, or access is refused.
    #[error("{0}")]
    Fatal(String),
}

/// A result that is still on its way. Reading returns one, so that many
/// reads can be waited for at once and any of them given up.
pub type Pending<'a, T> = Pin<Box<dyn Future<Output = Result<T, Failure>> + Send + 'a>>;

/// What a place can do, so that the engine knows how it may be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// Any range of a file can be reached, in any order. Without this a
    /// file is one stream from its first byte to its last.
    pub random_access: bool,
}

/// A place data can be read from.
pub trait Source: fmt::Debug + Send + Sync {
    /// What reading from this place allows.
    fn capabilities(&self) -> Capabilities;

    /// Reads `len` bytes of `file`, starting at `offset`.
    ///
    /// The whole range is returned or the read fails: a place never
    /// answers with fewer bytes than were asked for.
    fn read(&self, file: usize, offset: u64, len: u32) -> Pending<'_, Vec<u8>>;
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
    /// holds. A chunk is reported only if the place has checked it against
    /// the tree's digests, so whatever an earlier transfer left behind is
    /// kept if it is right and fetched again if it is not. `examined` is
    /// called with the size of each chunk looked at.
    ///
    /// # Errors
    ///
    /// Returns a failure if the place cannot be made ready.
    fn begin(&self, examined: &(dyn Fn(u64) + Sync)) -> Result<Bitfield, Failure>;

    /// Writes `data` into `file` at `offset`. Nothing of it is visible
    /// under the file's final name until [`Sink::commit`].
    ///
    /// # Errors
    ///
    /// Returns a failure if the bytes cannot be written.
    fn write(&self, file: usize, offset: u64, data: &[u8]) -> Result<(), Failure>;

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

    /// Finishes the tree once every file is committed, and makes what was
    /// committed last through a power cut.
    ///
    /// # Errors
    ///
    /// Returns a failure if the tree cannot be finished.
    fn finish(&self) -> Result<(), Failure>;
}
