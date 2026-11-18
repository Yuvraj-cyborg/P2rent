//! `mux probe`: say what is at a place, without copying it.

use super::tell::{self, Digest, Holder, Line};
use super::{Reach, cp, ui};
use anyhow::Context;
use mux::artifact::{DEFAULT_CHUNK_SIZE, FileDigest};
use mux::swarm::Ticket;
use mux::{Location, engine};
use std::path::Path;

#[derive(clap::Args)]
pub(super) struct Args {
    /// The place to look at: a web address, a file, a folder or a ticket
    #[arg(value_name = "PLACE")]
    place: String,

    /// The digest the place is said to have, to see how far a copy of it
    /// would be checked
    #[arg(long, value_name = "DIGEST")]
    hash: Option<FileDigest>,

    #[command(flatten)]
    reach: Reach,
}

pub(super) async fn run(args: &Args, json: bool, key: Option<&Path>) -> anyhow::Result<()> {
    let place = mux::locate(&args.place)?;
    if let Location::Ticket(ticket) = &place {
        return on_other_machines(args, ticket, json, key)
            .await
            .with_context(|| format!("cannot look at {place}"));
    }
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
            holders: None,
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

/// What a ticket names, and how much of it each machine the ticket gives
/// holds.
async fn on_other_machines(
    args: &Args,
    ticket: &Ticket,
    json: bool,
    key: Option<&Path>,
) -> anyhow::Result<()> {
    let node = super::node(key, &args.reach).await?;
    let found = mux::swarm::find(&node, ticket).await;
    let holders = match &found {
        Ok(found) => mux::swarm::holders(&node, found).await,
        Err(_) => Vec::new(),
    };
    node.shutdown().await;
    let found = found?;

    let artifact = &found.artifact;
    let kind = if artifact.is_file() { "file" } else { "folder" };
    let files = artifact.entries().iter().filter(|e| e.is_file()).count() as u64;
    let bytes = artifact.total_size();
    if json {
        tell::say(&Line::Probe {
            place: &args.place,
            name: artifact.name(),
            kind,
            bytes,
            files,
            // Every chunk of what a ticket names has its digest.
            checked: "verified",
            parts: true,
            digests: Vec::new(),
            holders: Some(
                holders
                    .iter()
                    .map(|holder| Holder {
                        node: holder.node.to_string(),
                        bytes: holder.bytes,
                    })
                    .collect(),
            ),
        });
        return Ok(());
    }
    println!("name     {}", artifact.name());
    println!("kind     {kind}");
    println!("size     {bytes} bytes ({})", ui::bytes(bytes));
    println!("files    {files}");
    println!("checked  verified, if it is copied");
    println!("parts    read several at once");
    for holder in &holders {
        let holds = match holder.bytes {
            Some(held) if held == bytes => "all of it".to_owned(),
            Some(held) => format!("{} of it", ui::bytes(held)),
            None => "could not be asked".to_owned(),
        };
        println!("holder   {}  {holds}", holder.node.short());
    }
    Ok(())
}
