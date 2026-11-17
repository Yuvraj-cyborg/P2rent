//! `mux sync`: make a folder hold what a place holds.
//!
//! Today it says what it would do, and does none of it. What to do about
//! each path is decided in the library, by `mux::mirror`.

use super::cp;
use super::tell::{self, Line};
use super::ui;
use anyhow::{Context, bail};
use mux::artifact::{DEFAULT_CHUNK_SIZE, Tree};
use mux::mirror::{self, Plan, Step};
use mux::{Location, engine, store};
use std::path::PathBuf;

#[derive(clap::Args)]
pub(super) struct Args {
    /// The place to match: a web address, a file or a folder
    #[arg(value_name = "SOURCE")]
    source: String,

    /// The folder, or the file, to make match it
    #[arg(value_name = "DESTINATION")]
    destination: String,

    /// Take away what the destination has and the source has not
    #[arg(long)]
    delete: bool,

    /// Say what would be done, and do none of it
    #[arg(long)]
    dry_run: bool,
}

pub(super) async fn run(args: &Args, json: bool) -> anyhow::Result<()> {
    if !args.dry_run {
        bail!(
            "`sync` can say what it would do and cannot do it yet: add --dry-run. \
             `mux cp` copies everything."
        );
    }
    let source = mux::locate(&args.source)?;
    let Location::Local(to) = mux::locate(&args.destination)? else {
        bail!("{} is not a folder on this machine", args.destination);
    };
    // The destination is what is made to match, not a folder to put a
    // copy in: so it is written here as a folder, for the check that it
    // does not lie inside the source.
    let inside = format!("{to}/");
    let sources = cp::sources(&source, Some(&inside), None, &[])
        .await
        .with_context(|| format!("cannot look at {source}"))?;
    let wanted = engine::plan(&sources, None, DEFAULT_CHUNK_SIZE)
        .await
        .with_context(|| format!("cannot look at {source}"))?;
    let wanted = store::as_kept(&wanted.tree)?;
    let there = there(PathBuf::from(&to))
        .await
        .with_context(|| format!("cannot look at {to}"))?;

    let plan = mirror::plan(&wanted, there.as_ref(), args.delete);
    say(args, json, &plan, &wanted, there.as_ref());
    Ok(())
}

/// What is at the destination now, as this system keeps it. Nothing, if
/// nothing is there.
async fn there(path: PathBuf) -> anyhow::Result<Option<Tree>> {
    let scanned =
        tokio::task::spawn_blocking(move || match store::scan(&path, DEFAULT_CHUNK_SIZE) {
            Ok(scanned) => store::as_kept(&scanned.tree).map(Some),
            Err(store::Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        });
    Ok(scanned.await.context("looking at it was cut short")??)
}

/// The word for a step.
fn word(step: Step) -> &'static str {
    match step {
        Step::Keep => "keep",
        Step::Add => "add",
        Step::Replace => "replace",
        Step::Touch => "touch",
        Step::Remove => "remove",
        Step::Blocked => "blocked",
    }
}

/// The path of an entry of a tree, if there is such a tree and entry.
fn path_of(tree: Option<&Tree>, entry: Option<usize>) -> Option<&str> {
    Some(tree?.entries().get(entry?)?.path.as_str())
}

/// Says what the plan would do: a line for each path where there is
/// something to do, and then how much of each there is.
fn say(args: &Args, json: bool, plan: &Plan, wanted: &Tree, there: Option<&Tree>) {
    for planned in &plan.steps {
        if planned.step == Step::Keep {
            continue;
        }
        let path =
            path_of(Some(wanted), planned.source).or_else(|| path_of(there, planned.destination));
        // The root has no path of its own.
        let path = match path {
            None | Some("") => ".",
            Some(path) => path,
        };
        let step = word(planned.step);
        if json {
            tell::say(&Line::Step { step, path });
        } else {
            println!("{step}\t{path}");
        }
    }

    let count = |step| plan.count(step);
    let bytes = plan.bytes(wanted);
    if json {
        tell::say(&Line::SyncPlan {
            source: &args.source,
            destination: &args.destination,
            add: count(Step::Add),
            replace: count(Step::Replace),
            touch: count(Step::Touch),
            remove: count(Step::Remove),
            keep: count(Step::Keep),
            blocked: count(Step::Blocked),
            bytes,
        });
        return;
    }
    if plan.is_done() {
        eprintln!("nothing to do: {} already matches", args.destination);
        return;
    }
    eprintln!(
        "would add {}, replace {}, touch {} and remove {}, and keep {}  ({} to bring)",
        count(Step::Add),
        count(Step::Replace),
        count(Step::Touch),
        count(Step::Remove),
        count(Step::Keep),
        ui::bytes(bytes),
    );
    if count(Step::Blocked) > 0 {
        eprintln!(
            "note: {} blocked: a folder is where the source has something else, and nothing \
             is removed without --delete",
            ui::plural(count(Step::Blocked), "path")
        );
    }
}
