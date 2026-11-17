//! `mux cp`: copy from one place to another.
//!
//! This file only reads the command line and shows what happens. What is
//! copied, how, and how far it is checked are the library's to decide.

use super::ui;
use anyhow::{Context, bail};
use mux::artifact::{DEFAULT_CHUNK_SIZE, FileDigest};
use mux::place::{Event, Observer, Source, Verification};
use mux::store::Folder;
use mux::{Location, engine};
use std::path::{MAIN_SEPARATOR, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(clap::Args)]
pub(super) struct Args {
    /// What to copy, and then where to put it
    #[arg(required = true, num_args = 2.., value_name = "PLACE")]
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

    /// Parts of a file to fetch at once from each source
    #[arg(long, default_value_t = 4, value_name = "N")]
    streams: usize,

    /// Give up after this long without receiving any data
    #[arg(long, default_value_t = 60, value_name = "SECONDS")]
    stall_timeout: u64,
}

pub(super) async fn run(args: Args) -> anyhow::Result<()> {
    let Some((to, from)) = args.places.split_last() else {
        bail!("say what to copy, and then where to put it");
    };
    if from.len() > 1 && (args.hash.is_some() || !args.mirror.is_empty()) {
        bail!(
            "--hash and --mirror say something of one source, and there are {}",
            from.len()
        );
    }
    let to = mux::locate(to)?;
    for source in from {
        let source = mux::locate(source)?;
        let sources = match (&source, &to) {
            #[cfg(feature = "web")]
            (Location::Web(url), Location::Local(_)) => on_the_web(url, &args).await,
            (Location::Local(path), Location::Local(to)) => in_a_folder(path, to, &args).await,
            _ => bail!(
                "`cp` cannot copy from {source} to {to} yet. It copies a web address or a \
                 folder into a folder. For another machine there are `mux seed` and `mux get`."
            ),
        };
        let Location::Local(to) = &to else {
            bail!("{to} is not a folder");
        };
        let sources = sources.with_context(|| format!("cannot copy from {source}"))?;
        copy(sources, to, &args)
            .await
            .with_context(|| format!("cannot copy from {source}"))?;
    }
    Ok(())
}

/// The file at a web address, and at each mirror of it.
#[cfg(feature = "web")]
async fn on_the_web(url: &str, args: &Args) -> anyhow::Result<Vec<Arc<dyn Source>>> {
    let asking = mux::web::Options {
        digest: args.hash,
        ..mux::web::Options::default()
    };
    let mut sources: Vec<Arc<dyn Source>> = Vec::new();
    for address in std::iter::once(url).chain(args.mirror.iter().map(String::as_str)) {
        let origin = mux::web::Origin::find(address, &asking)
            .await
            .with_context(|| format!("{address} cannot be used"))?;
        sources.push(Arc::new(origin));
    }
    Ok(sources)
}

/// The file or the folder at `path`, as it is now. It is looked at, and
/// none of it is read yet.
async fn in_a_folder(path: &str, to: &str, args: &Args) -> anyhow::Result<Vec<Arc<dyn Source>>> {
    if args.hash.is_some() || !args.mirror.is_empty() {
        bail!("--hash and --mirror are for a source on the web");
    }
    let from = PathBuf::from(path);
    let scanned = tokio::task::spawn_blocking(move || mux::store::scan(&from, DEFAULT_CHUNK_SIZE))
        .await
        .context("looking at the source was cut short")??;
    // A copy into the folder that is being copied would be copied too.
    let (into, _) = destination(to);
    if into
        .canonicalize()
        .is_ok_and(|into| into.starts_with(&scanned.root))
    {
        bail!("{to} is inside {path}: a folder cannot be copied into itself");
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

/// Copies what `sources` hold to `to`, and says what was done.
async fn copy(sources: Vec<Arc<dyn Source>>, to: &str, args: &Args) -> anyhow::Result<()> {
    let (folder, name) = destination(to);
    let limits = engine::Options {
        requests_per_supplier: args.streams.max(1),
        stall_timeout: Duration::from_secs(args.stall_timeout),
        ..engine::Options::default()
    };
    let started = Instant::now();
    let showing = Shown::new();
    let copied = engine::copy(
        sources,
        name,
        DEFAULT_CHUNK_SIZE,
        |tree| Arc::new(Folder::inside(tree.clone(), &folder)),
        limits,
        showing.observer(),
    )
    .await;
    showing.done();
    let copied = copied?;

    let saved = folder.join(copied.tree.name());
    let checked = match copied.verification {
        Some(Verification::Verified) => "verified",
        Some(Verification::Consistent) => "consistent",
        None => "already there",
    };
    // What was written is the only thing on standard output.
    println!(
        "{}\t{}\t{checked}",
        saved.display(),
        copied.tree.total_size()
    );
    eprintln!(
        "saved    {}  ({}, {checked})",
        saved.display(),
        ui::bytes(copied.tree.total_size())
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

/// The progress of one copy, as a bar on standard error.
struct Shown(indicatif::ProgressBar);

impl Shown {
    fn new() -> Self {
        Self(ui::bytes_bar(0, "copying"))
    }

    /// What moves the bar as the copy reports what it does.
    fn observer(&self) -> Observer {
        let bar = self.0.clone();
        Arc::new(move |event| match event {
            Event::Planned { bytes, .. } => bar.set_length(bytes),
            Event::Kept { bytes } => bar.set_position(bytes),
            Event::Stored { bytes, .. } => bar.inc(bytes),
            Event::Examined { .. }
            | Event::Opened { .. }
            | Event::Closed { .. }
            | Event::Committed { .. } => {}
        })
    }

    fn done(self) {
        self.0.finish_and_clear();
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
