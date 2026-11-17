//! `mux probe`: say what is at a place, without copying it.

use super::cp;
use super::tell::{self, Digest, Line};
use super::ui;
use anyhow::Context;
use mux::artifact::{DEFAULT_CHUNK_SIZE, FileDigest};
use mux::engine;

#[derive(clap::Args)]
pub(super) struct Args {
    /// The place to look at: a web address, a file or a folder
    #[arg(value_name = "PLACE")]
    place: String,

    /// The digest the place is said to have, to see how far a copy of it
    /// would be checked
    #[arg(long, value_name = "DIGEST")]
    hash: Option<FileDigest>,
}

pub(super) async fn run(args: &Args, json: bool) -> anyhow::Result<()> {
    let place = mux::locate(&args.place)?;
    let sources = cp::sources(&place, None, args.hash, &[])
        .await
        .with_context(|| format!("cannot look at {place}"))?;
    let planned = engine::plan(&sources, None, DEFAULT_CHUNK_SIZE)
        .await
        .with_context(|| format!("cannot look at {place}"))?;

    let tree = &planned.tree;
    let kind = if tree.is_file() { "file" } else { "folder" };
    let checked = cp::checked(Some(planned.verification()));
    let parts = sources
        .first()
        .is_some_and(|source| source.capabilities().random_access);
    let digests: Vec<Digest> = planned
        .digests()
        .filter_map(|(file, digest)| {
            Some(Digest {
                path: tree.entries().get(file)?.path.clone(),
                digest: digest.to_string(),
            })
        })
        .collect();
    let (bytes, files) = (tree.total_size(), planned.files());

    if json {
        tell::say(&Line::Probe {
            place: &args.place,
            name: tree.name(),
            kind,
            bytes,
            files,
            checked,
            parts,
            digests,
        });
        return Ok(());
    }
    println!("name     {}", tree.name());
    println!("kind     {kind}");
    println!("size     {bytes} bytes ({})", ui::bytes(bytes));
    println!("files    {files}");
    println!("checked  {checked}, if it is copied");
    println!(
        "parts    {}",
        if parts {
            "read several at once"
        } else {
            "read in order, as one stream"
        }
    );
    for known in &digests {
        match known.path.as_str() {
            "" => println!("digest   {}", known.digest),
            path => println!("digest   {}  {path}", known.digest),
        }
    }
    Ok(())
}
