//! `mux cp`: copy from one place to another.
//!
//! This file only reads the command line and shows what happens. What is
//! copied, how, and how far it is checked are the library's to decide.

use super::exit::misuse;
use super::tell::{self, Line};
use super::{Reach, seed, ui};
use anyhow::Context;
use mux::artifact::{DEFAULT_CHUNK_SIZE, FileDigest, Tree};
use mux::place::{Event, Observer, Source, Verification};
use mux::store::Folder;
use mux::swarm::{DownloadOptions, Node, Ticket};
use mux::{Location, engine};
use std::path::{MAIN_SEPARATOR, Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

#[derive(clap::Args)]
pub(super) struct Args {
    /// What to copy, and last of all where to put it: `peer:` offers it to
    /// other machines
    // Not `num_args = 2..`: that counts the places between two flags, and
    // would refuse `cp FROM --dry-run TO`.
    #[arg(required = true, value_name = "PLACE")]
    places: Vec<String>,

    /// The digest the source must have: `blake3:` or `sha256:` and 64 hex
    /// characters. The copy is then verified.
    #[arg(long, value_name = "DIGEST")]
    hash: Option<FileDigest>,

    /// Another address that holds the same file, to be read together with
    /// the source (repeatable). Needs a digest of the file: `--hash`, or
    /// one that a server gives.
    #[arg(long, value_name = "URL")]
    mirror: Vec<String>,

    /// The most parts of a file to fetch at once from each source
    #[arg(long, default_value_t = 8, value_name = "N")]
    streams: usize,

    /// Give up after this long without receiving any data
    #[arg(long, default_value_t = 60, value_name = "SECONDS")]
    stall_timeout: u64,

    /// Say what would be copied, and copy nothing
    #[arg(long)]
    dry_run: bool,

    #[command(flatten)]
    reach: Reach,
}

impl Args {
    /// Where to put the copies, and what to copy.
    fn places(&self) -> anyhow::Result<(&String, &[String])> {
        match self.places.split_last() {
            Some((to, from)) if !from.is_empty() => Ok((to, from)),
            _ => misuse("say what to copy, and then where to put it"),
        }
    }
}

pub(super) async fn run(args: Args, json: bool, key: Option<&Path>) -> anyhow::Result<()> {
    let (to, from) = args.places()?;
    if from.len() > 1 && (args.hash.is_some() || !args.mirror.is_empty()) {
        return misuse(format!(
            "--hash and --mirror say something of one source, and there are {}",
            from.len()
        ));
    }
    let to = mux::locate(to)?;
    if to == Location::Serve {
        return offer(args, json, key).await;
    }
    // Made when the first ticket is met: a copy that has to do with no
    // other machine opens no socket.
    let mut node: Option<Arc<Node>> = None;
    let mut outcome = Ok(());
    for source in from {
        let source = mux::locate(source)?;
        let Location::Local(to) = &to else {
            return misuse(format!(
                "`cp` cannot copy from {source} to {to}. It copies into a folder, or to \
                 `peer:`, which offers a file or a folder to other machines."
            ));
        };
        let from = source.to_string();
        let copy = Copy {
            from: &from,
            to,
            args: &args,
            json,
        };
        let copied = match &source {
            // Asked before a node is made: a command that is refused opens
            // no socket and gives this machine no key.
            Location::Ticket(ticket) => match copy.folder_for_a_ticket() {
                Ok(folder) => {
                    let node = match &node {
                        Some(node) => node.clone(),
                        None => node.insert(super::node(key, &args.reach).await?).clone(),
                    };
                    copy.fetch(ticket, &node, &folder).await
                }
                Err(refused) => Err(refused),
            },
            _ => copy.run(&source).await,
        };
        outcome = copied.with_context(|| format!("cannot copy from {source}"));
        if outcome.is_err() {
            break;
        }
    }
    if let Some(node) = node {
        node.shutdown().await;
    }
    outcome
}

/// `mux cp PATH... peer:`: offers the paths to other machines, as `mux
/// seed` does, until it is stopped.
async fn offer(args: Args, json: bool, key: Option<&Path>) -> anyhow::Result<()> {
    if args.dry_run {
        return misuse(
            "--dry-run has nothing to say of `peer:`: offering changes nothing on this machine",
        );
    }
    if args.hash.is_some() || !args.mirror.is_empty() {
        return misuse(
            "--hash and --mirror are for a source on the web, and `peer:` is offered files",
        );
    }
    let (_, from) = args.places()?;
    let mut paths = Vec::with_capacity(from.len());
    for source in from {
        match mux::locate(source)? {
            Location::Local(path) => paths.push(PathBuf::from(path)),
            other => {
                return misuse(format!(
                    "{other} cannot be offered to other machines with `cp`: a file or a \
                     folder on this machine can. To pass on what a ticket names while \
                     fetching it, there is `mux get --seed`."
                ));
            }
        }
    }
    seed::run(seed::Args::of(paths, args.reach), key, json).await
}

/// What `location` holds, as sources to copy from. `to` is where the copy
/// is to go, if there is to be one.
pub(super) async fn sources(
    location: &Location,
    to: Option<&str>,
    hash: Option<FileDigest>,
    mirrors: &[String],
) -> anyhow::Result<Vec<Arc<dyn Source>>> {
    match location {
        #[cfg(feature = "web")]
        Location::Web(url) => on_the_web(url, hash, mirrors).await,
        Location::Local(path) if hash.is_some() || !mirrors.is_empty() => misuse(format!(
            "--hash and --mirror are for a source on the web, and {path} is not"
        )),
        Location::Local(path) => in_a_folder(path, to).await,
        Location::Ticket(_) => {
            misuse("a ticket cannot be read here yet. `mux cp TICKET FOLDER` copies what it names.")
        }
        other => misuse(format!(
            "{other} is a place to copy to, and cannot be copied from"
        )),
    }
}

/// The file at a web address, and at each mirror of it.
#[cfg(feature = "web")]
async fn on_the_web(
    url: &str,
    digest: Option<FileDigest>,
    mirrors: &[String],
) -> anyhow::Result<Vec<Arc<dyn Source>>> {
    let asking = mux::web::Options {
        digest,
        ..mux::web::Options::default()
    };
    let mut sources: Vec<Arc<dyn Source>> = Vec::new();
    for address in std::iter::once(url).chain(mirrors.iter().map(String::as_str)) {
        let origin = mux::web::Origin::find(address, &asking)
            .await
            .with_context(|| format!("{address} cannot be used"))?;
        sources.push(Arc::new(origin));
    }
    Ok(sources)
}

/// The file or the folder at `path`, as it is now. It is looked at, and
/// none of it is read yet.
async fn in_a_folder(path: &str, to: Option<&str>) -> anyhow::Result<Vec<Arc<dyn Source>>> {
    let from = PathBuf::from(path);
    let scanned = tokio::task::spawn_blocking(move || mux::store::scan(&from, DEFAULT_CHUNK_SIZE))
        .await
        .context("looking at the source was cut short")??;
    // A copy into the folder that is being copied would be copied too.
    let into = to.map(|to| destination(to).0);
    let inside = into.and_then(|into| into.canonicalize().ok());
    if inside.is_some_and(|into| into.starts_with(&scanned.root)) {
        return misuse(format!(
            "the destination is inside {path}: a folder cannot be copied into itself"
        ));
    }
    if scanned.skipped > 0 {
        eprintln!(
            "note: left out {} that cannot be copied (special files, links that leave the folder, unfinished copies)",
            ui::plural(scanned.skipped, "thing")
        );
    }
    let folder = Folder::whole(Arc::new(scanned.tree), scanned.root);
    Ok(vec![Arc::new(folder)])
}

/// How far a copy was or would be checked, in the words a result uses.
pub(super) fn checked(verification: Option<Verification>) -> &'static str {
    match verification {
        Some(Verification::Verified) => "verified",
        Some(Verification::Consistent) => "consistent",
        None => "already there",
    }
}

/// One copy: from one source, as it was written, to the destination.
struct Copy<'a> {
    from: &'a str,
    to: &'a str,
    args: &'a Args,
    json: bool,
}

impl Copy<'_> {
    async fn run(&self, source: &Location) -> anyhow::Result<()> {
        let args = self.args;
        let sources = sources(source, Some(self.to), args.hash, &args.mirror).await?;
        if args.dry_run {
            self.plan(&sources).await
        } else {
            self.copy(sources).await
        }
    }

    /// The folder to put what a ticket names in, if the command line allows
    /// the copy.
    fn folder_for_a_ticket(&self) -> anyhow::Result<PathBuf> {
        if self.args.hash.is_some() || !self.args.mirror.is_empty() {
            return misuse(
                "--hash and --mirror are for a source on the web: a ticket says what its \
                 content has to be",
            );
        }
        match destination(self.to) {
            (folder, None) => Ok(folder),
            (_, Some(name)) => misuse(format!(
                "what a ticket names keeps its own name, and cannot be copied as {name:?}. \
                 Name the folder to put it in, with a `/` at its end if it is not there yet."
            )),
        }
    }

    /// Copies what a ticket names from the machines that hold it into
    /// `folder`. Every chunk is checked against the ticket's manifest, so
    /// the copy is verified.
    async fn fetch(&self, ticket: &Ticket, node: &Arc<Node>, folder: &Path) -> anyhow::Result<()> {
        let args = self.args;
        let finding = (!self.json).then(|| ui::spinner("finding the machines that hold it"));
        let found = mux::swarm::find(node, ticket).await;
        if let Some(finding) = finding {
            finding.finish_and_clear();
        }
        let found = found?;
        let artifact = found.artifact.clone();
        let saved = folder.join(artifact.name());
        let bytes = artifact.total_size();
        let checked = checked(Some(Verification::Verified));
        if args.dry_run {
            let files = artifact.entries().iter().filter(|e| e.is_file()).count() as u64;
            self.say_plan(&saved, bytes, files, checked);
            return Ok(());
        }

        let limits = DownloadOptions {
            stall_timeout: Duration::from_secs(args.stall_timeout),
            ..DownloadOptions::default()
        };
        let showing = Shown::new(self.json, self.from, self.to, folder);
        showing.copying(artifact.tree());
        let progress = Arc::new(mux::swarm::Progress::default());
        let fetching = mux::swarm::fetch(node, found, folder, limits, progress, showing.observer());
        let fetched = fetching.await;
        showing.done();
        let report = fetched?;

        if self.json {
            tell::say(&Line::Done {
                path: &saved.to_string_lossy(),
                bytes,
                checked,
                fetched: report.fetched_bytes,
                kept: report.resumed_bytes,
            });
            return Ok(());
        }
        println!("{}\t{bytes}\t{checked}", saved.display());
        eprintln!(
            "saved    {}  ({}, {checked})",
            saved.display(),
            ui::bytes(bytes)
        );
        if report.fetched_bytes > 0 {
            eprintln!(
                "fetched  {} in {:.1}s ({}) from {}",
                ui::bytes(report.fetched_bytes),
                report.elapsed.as_secs_f64(),
                ui::rate(report.fetched_bytes, report.elapsed),
                ui::plural(report.sources.len(), "machine"),
            );
        }
        if report.resumed_bytes > 0 {
            eprintln!(
                "kept     {} that was already there",
                ui::bytes(report.resumed_bytes)
            );
        }
        if report.skipped_links > 0 {
            eprintln!(
                "skipped  {}, which this system cannot create",
                ui::plural(report.skipped_links, "link")
            );
        }
        Ok(())
    }

    /// Says what a copy would bring to `path`, as a dry run says it.
    fn say_plan(&self, path: &Path, bytes: u64, files: u64, checked: &str) {
        if self.json {
            tell::say(&Line::Plan {
                source: self.from,
                destination: self.to,
                path: &path.to_string_lossy(),
                bytes,
                files,
                checked,
            });
        } else {
            println!("{}\t{bytes}\t{checked}", path.display());
            eprintln!(
                "would copy  {}  ({}, {}, {checked})",
                path.display(),
                ui::bytes(bytes),
                ui::plural(files, "file"),
            );
        }
    }

    /// Says what the copy would do, and does none of it.
    async fn plan(&self, sources: &[Arc<dyn Source>]) -> anyhow::Result<()> {
        let (folder, name) = destination(self.to);
        let planned = engine::plan(sources, name, DEFAULT_CHUNK_SIZE).await?;
        let tree = &planned.tree;
        let place = Arc::new(Folder::inside(tree.clone(), &folder));
        let there = planned.is_in(&place).await?;
        let checked = checked((!there).then(|| planned.verification()));
        let path = folder.join(tree.name());
        self.say_plan(&path, tree.total_size(), planned.files(), checked);
        Ok(())
    }

    /// Copies what `sources` hold, and says what was done.
    async fn copy(&self, sources: Vec<Arc<dyn Source>>) -> anyhow::Result<()> {
        let (folder, name) = destination(self.to);
        let limits = engine::Options {
            requests_per_supplier: self.args.streams.max(1),
            stall_timeout: Duration::from_secs(self.args.stall_timeout),
            ..engine::Options::default()
        };
        let started = Instant::now();
        let showing = Shown::new(self.json, self.from, self.to, &folder);
        let copied = engine::copy(
            sources,
            name,
            DEFAULT_CHUNK_SIZE,
            |tree| {
                showing.copying(tree);
                Arc::new(Folder::inside(tree.clone(), &folder))
            },
            limits,
            showing.observer(),
        )
        .await;
        showing.done();
        let copied = copied?;

        let saved = folder.join(copied.tree.name());
        let checked = checked(copied.verification);
        let bytes = copied.tree.total_size();
        if self.json {
            tell::say(&Line::Done {
                path: &saved.to_string_lossy(),
                bytes,
                checked,
                fetched: copied.fetched,
                kept: copied.resumed,
            });
            return Ok(());
        }
        // What was written is the only thing on standard output.
        println!("{}\t{bytes}\t{checked}", saved.display());
        eprintln!(
            "saved    {}  ({}, {checked})",
            saved.display(),
            ui::bytes(bytes)
        );
        if copied.fetched > 0 {
            let elapsed = started.elapsed();
            eprintln!(
                "fetched  {} in {:.1}s ({})",
                ui::bytes(copied.fetched),
                elapsed.as_secs_f64(),
                ui::rate(copied.fetched, elapsed),
            );
        }
        if copied.resumed > 0 {
            eprintln!(
                "kept     {} that was already there",
                ui::bytes(copied.resumed)
            );
        }
        Ok(())
    }
}

/// Where a copy goes, as `cp` has it: into `to` if that is a folder or is
/// written as one, under the name the source has. And otherwise at `to`
/// itself, which is then the name the copy is given.
fn destination(to: &str) -> (PathBuf, Option<String>) {
    let path = Path::new(to);
    let written_as_folder = to.ends_with(['/', MAIN_SEPARATOR]) || to == "." || to == "..";
    if written_as_folder || path.is_dir() {
        return (path.to_owned(), None);
    }
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned());
    let folder = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_owned(),
        _ => PathBuf::from("."),
    };
    (folder, name)
}

/// How often a program is told how far a copy has got.
const PROGRESS_EVERY: Duration = Duration::from_millis(200);

/// The progress of one copy: a bar on standard error for a person, or
/// lines on standard output for a program.
#[derive(Clone)]
pub(super) enum Shown {
    Bar(indicatif::ProgressBar),
    Lines(Arc<Lines>),
}

/// What the lines of one copy are made from.
pub(super) struct Lines {
    source: String,
    destination: String,
    /// The folder the copy goes into.
    folder: PathBuf,
    /// The tree being copied, once it is known.
    tree: OnceLock<Arc<Tree>>,
    progress: Mutex<Progress>,
}

/// How far a copy has got, and when that was last said.
struct Progress {
    bytes: u64,
    of: u64,
    said: Instant,
}

impl Shown {
    /// The progress of a copy from `from` to `to`, as they were typed,
    /// which goes into `folder`: as lines if `json`, and as a bar if not.
    pub(super) fn new(json: bool, from: &str, to: &str, folder: &Path) -> Self {
        if !json {
            return Self::Bar(ui::bytes_bar(0, "copying"));
        }
        Self::Lines(Arc::new(Lines {
            source: from.to_owned(),
            destination: to.to_owned(),
            folder: folder.to_owned(),
            tree: OnceLock::new(),
            progress: Mutex::new(Progress {
                bytes: 0,
                of: 0,
                said: Instant::now(),
            }),
        }))
    }

    /// Notes the tree that is being copied, to name its files by.
    fn copying(&self, tree: &Arc<Tree>) {
        if let Self::Lines(lines) = self {
            let _ = lines.tree.set(tree.clone());
        }
    }

    /// What shows each thing the copy reports.
    pub(super) fn observer(&self) -> Observer {
        let shown = self.clone();
        Arc::new(move |event| match &shown {
            Self::Bar(bar) => match event {
                Event::Planned { bytes, .. } => bar.set_length(bytes),
                Event::Kept { bytes } => bar.set_position(bytes),
                Event::Stored { bytes, .. } => bar.inc(bytes),
                _ => {}
            },
            Self::Lines(lines) => lines.say(event),
        })
    }

    pub(super) fn done(self) {
        if let Self::Bar(bar) = self {
            bar.finish_and_clear();
        }
    }
}

impl Lines {
    fn say(&self, event: Event) {
        match event {
            Event::Planned { bytes, files } => {
                self.progress().of = bytes;
                tell::say(&Line::Start {
                    source: &self.source,
                    destination: &self.destination,
                    bytes,
                    files,
                });
            }
            Event::Kept { bytes } | Event::Stored { bytes, .. } => self.arrived(bytes),
            Event::Committed { file } => {
                let Some(tree) = self.tree.get() else { return };
                let Some(entry) = tree.entries().get(file) else {
                    return;
                };
                let mut path = self.folder.join(tree.name());
                path.extend(entry.path.split('/').filter(|part| !part.is_empty()));
                tell::say(&Line::File {
                    path: &path.to_string_lossy(),
                });
            }
            _ => {}
        }
    }

    fn progress(&self) -> std::sync::MutexGuard<'_, Progress> {
        // A lock is poisoned only if a holder panicked, and what it holds
        // is numbers that are right either way.
        self.progress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Counts bytes that are now at the destination, and says how far the
    /// copy has got if it has not said so lately, or if that is all of it.
    fn arrived(&self, bytes: u64) {
        let mut progress = self.progress();
        progress.bytes += bytes;
        let whole = progress.bytes >= progress.of;
        if bytes > 0 && (whole || progress.said.elapsed() >= PROGRESS_EVERY) {
            progress.said = Instant::now();
            tell::say(&Line::Progress {
                bytes: progress.bytes,
                of: progress.of,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_copy_goes_into_a_folder_or_takes_the_name_it_is_given() {
        let tmp = tempfile::tempdir().unwrap();
        let here = tmp.path().to_str().unwrap();
        let into = |to: &str| destination(to);

        // A folder that is there, however it is written.
        assert_eq!(into(here), (PathBuf::from(here), None));
        assert_eq!(into("."), (PathBuf::from("."), None));
        // One that is not there yet, written as a folder.
        let new = format!("{here}/new/");
        assert_eq!(into(&new), (PathBuf::from(&new), None));
        // Anything else is the name of the copy, in the folder before it.
        let named = format!("{here}/model.bin");
        assert_eq!(
            into(&named),
            (PathBuf::from(here), Some("model.bin".to_owned()))
        );
        assert_eq!(
            into("model.bin"),
            (PathBuf::from("."), Some("model.bin".to_owned()))
        );
    }
}
