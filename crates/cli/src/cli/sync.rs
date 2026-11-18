//! `mux sync`: make a folder hold what a place holds.
//!
//! This file reads the command line and says what happened. What to do
//! about each path is decided by `mux::mirror`, and it is carried out by
//! `mux::sync`.

use super::cp::{self, Shown};
use super::exit::misuse;
use super::tell::{self, Line};
use super::ui;
use anyhow::{Context, bail};
use mux::Location;
use mux::artifact::Tree;
use mux::engine;
use mux::mirror::Step;
use mux::sync::{Looked, Options, look, sync};
use std::path::Path;
use std::time::Duration;

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

    #[command(flatten)]
    care: Care,

    /// The most parts of a file to fetch at once from the source
    #[arg(long, default_value_t = 8, value_name = "N")]
    streams: usize,

    /// Give up after this long without receiving any data
    #[arg(long, default_value_t = 60, value_name = "SECONDS")]
    stall_timeout: u64,
}

/// The two rules that keep a sync from losing what a person would want
/// back, and how to set each aside.
#[derive(clap::Args)]
struct Care {
    /// Keep nothing of what is taken away or replaced. Without this it is
    /// moved to a folder named `.mux-trash` inside the destination.
    #[arg(long)]
    no_trash: bool,

    /// Go on though more than half of what the destination has would be
    /// removed
    #[arg(long)]
    force: bool,
}

/// Whether a sync was carried out, or only looked at.
#[derive(Clone, Copy)]
enum Told<'a> {
    /// It was only looked at.
    Planned,
    /// It was carried out, and what it took away is in `trash`, if it took
    /// anything away and kept it.
    Done { trash: Option<&'a str> },
}

pub(super) async fn run(args: &Args, json: bool) -> anyhow::Result<()> {
    let source = mux::locate(&args.source)?;
    let Location::Local(to) = mux::locate(&args.destination)? else {
        return misuse(format!(
            "{} is not a folder on this machine",
            args.destination
        ));
    };
    // The destination is what is made to match, not a folder to put a
    // copy in: so it is written here as a folder, for the check that it
    // does not lie inside the source.
    let inside = format!("{to}/");
    let sources = cp::sources(&source, Some(&inside), None, &[])
        .await
        .with_context(|| format!("cannot look at {source}"))?;
    let to = Path::new(&to);

    if args.dry_run {
        let looked = look(&sources, to, args.delete).await?;
        say(args, json, &looked, Told::Planned);
        if let Some(why) = looked.stopped(args.care.force) {
            eprintln!("note: a sync would stop here: {why}");
        }
        return Ok(());
    }

    let options = Options {
        remove: args.delete,
        trash: !args.care.no_trash,
        force: args.care.force,
        limits: engine::Options {
            requests_per_supplier: args.streams.max(1),
            stall_timeout: Duration::from_secs(args.stall_timeout),
            ..engine::Options::default()
        },
    };
    let folder = to.parent().unwrap_or(Path::new("."));
    let showing = Shown::new(json, &args.source, &args.destination, folder);
    let synced = sync(sources, to, &options, showing.observer()).await;
    showing.done();
    let synced = synced?;

    let trash = synced.trash.as_deref().map(Path::to_string_lossy);
    let trash = trash.as_deref();
    say(args, json, &synced.looked, Told::Done { trash });
    let blocked = synced.looked.plan.count(Step::Blocked);
    if blocked > 0 {
        bail!(
            "{} left as it was: a folder is where the source has something else. With \
             --delete the folder is replaced.",
            ui::plural(blocked, "path")
        );
    }
    Ok(())
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

/// Says what there was to do: a line for each path where there was
/// something, and then how much of each.
fn say(args: &Args, json: bool, looked: &Looked, told: Told<'_>) {
    let (plan, wanted, was) = (&looked.plan, &looked.wanted, looked.was.as_ref());
    for planned in &plan.steps {
        if planned.step == Step::Keep {
            continue;
        }
        let path =
            path_of(Some(wanted), planned.source).or_else(|| path_of(was, planned.destination));
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
        let (source, destination) = (args.source.as_str(), args.destination.as_str());
        let (add, replace, touch) = (count(Step::Add), count(Step::Replace), count(Step::Touch));
        let (remove, keep) = (count(Step::Remove), count(Step::Keep));
        let blocked = count(Step::Blocked);
        tell::say(&match told {
            Told::Done { trash } => Line::Synced {
                source,
                destination,
                add,
                replace,
                touch,
                remove,
                keep,
                blocked,
                bytes,
                trash,
            },
            Told::Planned => Line::SyncPlan {
                source,
                destination,
                add,
                replace,
                touch,
                remove,
                keep,
                blocked,
                bytes,
            },
        });
        return;
    }
    if plan.is_done() {
        eprintln!("nothing to do: {} already matches", args.destination);
        return;
    }
    // The same counts, as what was done or as what would be.
    let words = match told {
        Told::Done { .. } => ["added", "replaced", "touched", "removed", "brought"],
        Told::Planned => ["would add", "replace", "touch", "remove", "to bring"],
    };
    eprintln!(
        "{} {}, {} {}, {} {} and {} {}, and kept {}  ({} {})",
        words[0],
        count(Step::Add),
        words[1],
        count(Step::Replace),
        words[2],
        count(Step::Touch),
        words[3],
        count(Step::Remove),
        count(Step::Keep),
        ui::bytes(bytes),
        words[4],
    );
    match told {
        Told::Done { trash: Some(trash) } => {
            eprintln!("what was replaced or removed is in {trash}");
        }
        Told::Planned if count(Step::Blocked) > 0 => eprintln!(
            "note: {} blocked: a folder is where the source has something else, and nothing \
             is removed without --delete",
            ui::plural(count(Step::Blocked), "path")
        ),
        _ => {}
    }
}
