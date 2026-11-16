//! Helpers shared by the integration tests.
#![allow(dead_code)]

use p2rent_core::artifact::{Artifact, MIN_CHUNK_SIZE};
use p2rent_fs as store;
use p2rent_peer::net::{Identity, PeerInfo};
use p2rent_peer::swarm::{DownloadOptions, Node, Progress, Report, Ticket};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub(crate) const CHUNK: usize = MIN_CHUNK_SIZE as usize;

pub(crate) fn localhost() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

/// A node with a fresh identity on an ephemeral local port.
pub(crate) fn node() -> Arc<Node> {
    Node::bind(&Identity::generate().unwrap(), localhost()).unwrap()
}

pub(crate) fn peer_info(node: &Node) -> PeerInfo {
    PeerInfo {
        node: node.node_id(),
        addr: node.local_addr().unwrap(),
    }
}

/// Deterministic, non-repeating test data.
pub(crate) fn pattern(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[0]
        })
        .collect()
}

pub(crate) fn index(path: &Path) -> (Arc<Artifact>, PathBuf) {
    let indexed = store::index(path, MIN_CHUNK_SIZE, &|_| {}).unwrap();
    (Arc::new(indexed.artifact), indexed.root)
}

/// Indexes `path`, seeds it from `node` and returns a ticket pointing there.
pub(crate) fn seed(node: &Node, path: &Path) -> (Arc<Artifact>, Ticket) {
    let (artifact, root) = index(path);
    node.seed(artifact.clone(), root);
    let ticket = ticket_for(node, &artifact);
    (artifact, ticket)
}

pub(crate) fn ticket_for(node: &Node, artifact: &Artifact) -> Ticket {
    node.ticket(artifact.id(), &[node.local_addr().unwrap().to_string()])
}

pub(crate) fn options() -> DownloadOptions {
    DownloadOptions {
        stall_timeout: Duration::from_secs(10),
        ..DownloadOptions::default()
    }
}

pub(crate) async fn get(
    node: &Arc<Node>,
    ticket: &Ticket,
    out: &Path,
) -> p2rent_peer::Result<Report> {
    p2rent_peer::swarm::download(node, ticket, out, options(), Arc::new(Progress::default())).await
}

/// Asserts that two directory trees hold the same files with the same bytes.
pub(crate) fn assert_same_tree(expected: &Path, actual: &Path) {
    let list = |root: &Path| -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = walk(root)
            .into_iter()
            .map(|p| p.strip_prefix(root).unwrap().to_owned())
            .collect();
        files.sort();
        files
    };
    let files = list(expected);
    assert_eq!(files, list(actual), "file lists differ");
    for file in files {
        assert_eq!(
            std::fs::read(expected.join(&file)).unwrap(),
            std::fs::read(actual.join(&file)).unwrap(),
            "{} differs",
            file.display()
        );
    }
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files.extend(walk(&path));
        } else {
            files.push(path);
        }
    }
    files
}
