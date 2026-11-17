//! Making a folder hold what a source holds.
//!
//! [`mirror`] decides what to do about each path. This
//! carries it out on a folder of this machine, in an order that loses
//! nothing: what is to go is moved to a trash folder first, then what is new
//! is copied in, each file under a part name until it is whole.

use crate::artifact::{DEFAULT_CHUNK_SIZE, EntryKind, Timestamp, Tree};
use crate::mirror::{self, Plan, Step};
use crate::place::{Observer, Source};
use crate::store::{self, Folder, Trash};
use crate::{engine, engine::Copied};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// How a sync is to be carried out.
#[derive(Debug, Clone)]
pub struct Options {
    /// Take away what the destination has and the source has not.
    pub remove: bool,
    /// Keep what is taken away or replaced in a trash folder inside the
    /// destination.
    pub trash: bool,
    /// Go on though more than half of what the destination has would be
    /// removed.
    pub force: bool,
    /// Limits for the copying.
    pub limits: engine::Options,
}

/// What a sync would do: the two places as they are, and the step for
/// each path.
#[derive(Debug)]
pub struct Looked {
    /// The step for each path.
    pub plan: Plan,
    /// What the source holds, as this system keeps it. The plan's steps
    /// name its entries.
    pub wanted: Tree,
    /// What the destination holds, if it holds anything.
    pub was: Option<Tree>,
    /// The destination, as a path from the root of the file system.
    destination: PathBuf,
}

impl Looked {
    /// Why this sync is not to be carried out, if a rule forbids it. With
    /// `force`, removing more than half of what is there is allowed.
    pub fn stopped(&self, force: bool) -> Option<String> {
        let to = self.destination.display();
        let was = self.was.as_ref()?;
        if was.is_file() != self.wanted.is_file() {
            let kind = |tree: &Tree| if tree.is_file() { "a file" } else { "a folder" };
            return Some(format!(
                "the source is {} and {to} is {}: sync a file with a file, or a folder with a \
                 folder",
                kind(&self.wanted),
                kind(was),
            ));
        }
        (self.plan.removes_most() && !force).then(|| {
            format!(
                "this would remove {} of the {} things in {to}, which is more than half. If \
                 the source is what it should be, say so with --force.",
                self.plan.count(Step::Remove),
                was.entries().len().saturating_sub(1),
            )
        })
    }
}

/// What a sync did.
#[derive(Debug)]
pub struct Synced {
    /// What there was to do.
    pub looked: Looked,
    /// What was copied. `None` if nothing had to be.
    pub copied: Option<Copied>,
    /// Where what was taken away or replaced was put, if anything was.
    pub trash: Option<PathBuf>,
}

/// Why a sync was not carried out.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A rule stopped it before anything was changed. The text says which,
    /// and what to do.
    #[error("{0}")]
    Stopped(String),
    /// The source could not be looked at, or the copying failed.
    #[error(transparent)]
    Copy(#[from] engine::Error),
    /// The destination could not be looked at.
    #[error(transparent)]
    Scan(#[from] store::Error),
    /// Something at the destination could not be moved, removed or changed.
    #[error("cannot change {}: {source}", path.display())]
    Change {
        /// What could not be changed.
        path: PathBuf,
        /// Why.
        source: std::io::Error,
    },
}

/// Looks at what `sources` hold and at what `destination` holds, and works
/// out what a sync would do. Nothing is changed.
///
/// The destination is the folder, or the file, that is made to match: it
/// is not a folder to put a copy in. With `remove` unset, nothing that the
/// destination has and the source has not is to be taken away.
///
/// # Errors
///
/// Returns an error if either place cannot be looked at.
pub async fn look(
    sources: &[Arc<dyn Source>],
    destination: &Path,
    remove: bool,
) -> Result<Looked, Error> {
    let destination = std::path::absolute(destination).map_err(|source| Error::Change {
        path: destination.to_owned(),
        source,
    })?;
    let Some(name) = destination.file_name() else {
        let said = format!("{} has no name to sync under", destination.display());
        return Err(Error::Stopped(said));
    };
    let name = name.to_string_lossy().into_owned();
    let wanted = engine::plan(sources, Some(name), DEFAULT_CHUNK_SIZE)
        .await
        .map_err(engine::Error::from)?;
    let wanted = store::as_kept(&wanted.tree)?;
    let was = there(destination.clone()).await?;
    Ok(Looked {
        plan: mirror::plan(&wanted, was.as_ref(), remove),
        wanted,
        was,
        destination,
    })
}

/// Makes `destination` hold what `sources` hold. Each source holds the
/// same files, as for [`engine::copy`].
///
/// # Errors
///
/// Returns [`Error::Stopped`] if a rule forbids the sync, before anything
/// is changed, and the other errors if it fails part of the way. A sync
/// that failed is carried on with by running it again.
pub async fn sync(
    sources: Vec<Arc<dyn Source>>,
    destination: &Path,
    options: &Options,
    observer: Observer,
) -> Result<Synced, Error> {
    let looked = look(&sources, destination, options.remove).await?;
    if let Some(why) = looked.stopped(options.force) {
        return Err(Error::Stopped(why));
    }
    let mut synced = Synced {
        looked,
        copied: None,
        trash: None,
    };
    if synced.looked.plan.is_done() {
        return Ok(synced);
    }
    let (destination, wanted) = (synced.looked.destination.clone(), &synced.looked.wanted);
    // Both are there: `look` found the name.
    let parent = destination.parent().unwrap_or(Path::new("")).to_owned();
    let name = wanted.name().to_owned();

    // A folder keeps its trash inside itself. A file has no inside, and its
    // trash is beside it.
    let (trash_in, below) = if wanted.is_file() {
        (parent.clone(), Some(name.clone()))
    } else {
        (destination.clone(), None)
    };
    let clearing = Clearing {
        root: trash_in.clone(),
        below,
        trash: options.trash.then(|| Trash::new(&trash_in, now())),
    };
    synced.trash = make_way(&synced.looked, clearing).await?;

    let picked = to_bring(&synced.looked.plan, &synced.looked.wanted);
    if !picked.is_empty() {
        let parts = sources
            .into_iter()
            .map(|source| engine::part(source, picked.clone()));
        let copied = engine::copy(
            parts.collect(),
            Some(name),
            DEFAULT_CHUNK_SIZE,
            |tree| Arc::new(Folder::inside(tree.clone(), &parent)),
            options.limits.clone(),
            observer,
        )
        .await?;
        synced.copied = Some(copied);
    }
    touch(&synced.looked, &destination).await?;
    Ok(synced)
}

/// What is at the destination now, as this system keeps it. Nothing, if
/// nothing is there.
async fn there(path: PathBuf) -> Result<Option<Tree>, Error> {
    let scanned =
        tokio::task::spawn_blocking(move || match store::scan(&path, DEFAULT_CHUNK_SIZE) {
            Ok(scanned) => store::as_kept(&scanned.tree).map(Some),
            Err(store::Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        });
    Ok(scanned
        .await
        .unwrap_or_else(|stopped| Err(cut_short(&stopped)))?)
}

fn cut_short(stopped: &tokio::task::JoinError) -> store::Error {
    store::Error::Io(std::io::Error::other(format!(
        "work on the destination was cut short: {stopped}"
    )))
}

fn now() -> Timestamp {
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    Timestamp {
        seconds: i64::try_from(since.as_secs()).unwrap_or(i64::MAX),
        nanos: since.subsec_nanos(),
    }
}

/// Where things are taken away from, and where they are put.
struct Clearing {
    /// The folder that paths are counted from.
    root: PathBuf,
    /// What every path lies below, if the destination is one file: its
    /// name.
    below: Option<String>,
    /// The trash, if one is kept.
    trash: Option<Trash>,
}

impl Clearing {
    /// The path of an entry, counted from the root.
    fn path(&self, entry: &str) -> String {
        match (&self.below, entry) {
            (Some(below), "") => below.clone(),
            (Some(below), entry) => format!("{below}/{entry}"),
            (None, entry) => entry.to_owned(),
        }
    }

    fn on_disk(&self, path: &str) -> PathBuf {
        let mut on_disk = self.root.clone();
        on_disk.extend(path.split('/').filter(|part| !part.is_empty()));
        on_disk
    }

    /// Takes away what is at `path`: into the trash, or for good.
    fn take(&self, path: &str) -> std::io::Result<()> {
        match &self.trash {
            Some(trash) => trash.take(path),
            None => store::remove(&self.on_disk(path)),
        }
    }

    /// Sees that the file at `path`, which is about to be replaced, is
    /// still to be had afterwards. It stays where it is until then.
    fn keep(&self, path: &str) -> std::io::Result<()> {
        match &self.trash {
            Some(trash) => trash.keep(path),
            None => Ok(()),
        }
    }
}

/// Takes away what the plan removes, and what stands where something of
/// another kind is to go. A file that another file replaces stays until
/// the new one is whole, and is kept in the trash as well.
///
/// Returns the trash folder, if anything was put in it.
async fn make_way(looked: &Looked, clearing: Clearing) -> Result<Option<PathBuf>, Error> {
    let Some(was) = &looked.was else {
        return Ok(None);
    };
    // What to do with what the destination has, by its path there.
    let mut to_take: Vec<String> = Vec::new();
    let mut to_keep: Vec<String> = Vec::new();
    for planned in &looked.plan.steps {
        let Some(old) = planned
            .destination
            .and_then(|entry| was.entries().get(entry))
        else {
            continue;
        };
        let new = planned
            .source
            .and_then(|entry| looked.wanted.entries().get(entry));
        let both_files = old.is_file() && new.is_some_and(crate::artifact::Entry::is_file);
        match planned.step {
            Step::Replace if both_files => to_keep.push(clearing.path(&old.path)),
            Step::Remove | Step::Replace => to_take.push(clearing.path(&old.path)),
            Step::Keep | Step::Add | Step::Touch | Step::Blocked => {}
        }
    }
    if to_take.is_empty() && to_keep.is_empty() {
        return Ok(None);
    }
    let work = tokio::task::spawn_blocking(move || {
        let failed = |path: &String, source| Error::Change {
            path: clearing.on_disk(path),
            source,
        };
        // In the order of the paths: a folder goes with all that is in
        // it, and what was in it is then no longer there to take.
        for path in &to_take {
            clearing.take(path).map_err(|e| failed(path, e))?;
        }
        for path in &to_keep {
            clearing.keep(path).map_err(|e| failed(path, e))?;
        }
        Ok(clearing.trash.map(|trash| trash.folder().to_owned()))
    });
    work.await
        .unwrap_or_else(|stopped| Err(Error::Scan(cut_short(&stopped))))
}

/// The entries of the source to copy: what the plan adds or replaces, and
/// every directory. A directory costs nothing to bring, and bringing it
/// puts its time right after what was done inside it.
fn to_bring(plan: &Plan, wanted: &Tree) -> Vec<usize> {
    let brought = |entry: usize| {
        plan.steps.iter().any(|planned| {
            planned.source == Some(entry) && matches!(planned.step, Step::Add | Step::Replace)
        })
    };
    let entries = wanted.entries().iter().enumerate();
    let picked: Vec<usize> = entries
        .filter(|(entry, described)| described.kind == EntryKind::Directory || brought(*entry))
        .map(|(entry, _)| entry)
        .collect();
    // One file that is neither added nor replaced leaves nothing to copy.
    let anything = picked.iter().any(|&entry| brought(entry));
    if wanted.is_file() && !anything {
        return Vec::new();
    }
    picked
}

/// Gives each file that the plan touches the permissions the source's has.
async fn touch(looked: &Looked, destination: &Path) -> Result<(), Error> {
    let mut touched = Vec::new();
    for planned in &looked.plan.steps {
        let entry = planned.source.filter(|_| planned.step == Step::Touch);
        let Some((described, recorded)) = entry.and_then(|entry| {
            Some((
                looked.wanted.entries().get(entry)?,
                looked.wanted.metadata(entry)?,
            ))
        }) else {
            continue;
        };
        if described.is_file() {
            let mut path = destination.to_owned();
            path.extend(described.path.split('/').filter(|part| !part.is_empty()));
            touched.push((path, recorded));
        }
    }
    if touched.is_empty() {
        return Ok(());
    }
    let work = tokio::task::spawn_blocking(move || {
        for (path, recorded) in touched {
            store::touch(&path, recorded).map_err(|source| Error::Change { path, source })?;
        }
        Ok(())
    });
    work.await
        .unwrap_or_else(|stopped| Err(Error::Scan(cut_short(&stopped))))
}
