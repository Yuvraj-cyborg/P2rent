//! Helpers shared by the integration tests.
#![allow(dead_code)]

use ::fs as store;
use model::artifact::{Artifact, MIN_CHUNK_SIZE};
use peer::net::{Identity, Options, PeerInfo, Relays};
use peer::swarm::{DownloadOptions, Node, Progress, Report, Ticket};
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub(crate) const CHUNK: usize = MIN_CHUNK_SIZE as usize;

pub(crate) fn localhost() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

/// A node reached at one local address and through no relay.
pub(crate) fn direct() -> Options {
    Options {
        listen: Some(localhost()),
        relays: Relays::Off,
        ..Options::default()
    }
}

/// The address a node made with [`direct`] listens on.
pub(crate) fn addr_of(node: &Node) -> SocketAddr {
    node.local_addrs()[0]
}

/// A node with a fresh identity on an ephemeral local port.
pub(crate) async fn node() -> Arc<Node> {
    Node::bind(&Identity::generate(), &direct()).await.unwrap()
}

pub(crate) fn peer_info(node: &Node) -> PeerInfo {
    PeerInfo::at(node.node_id(), addr_of(node))
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
    node.ticket(artifact.id(), &[addr_of(node).to_string()])
}

pub(crate) fn options() -> DownloadOptions {
    DownloadOptions {
        stall_timeout: Duration::from_secs(10),
        ..DownloadOptions::default()
    }
}

pub(crate) async fn get(node: &Arc<Node>, ticket: &Ticket, out: &Path) -> peer::Result<Report> {
    peer::swarm::download(node, ticket, out, options(), Arc::new(Progress::default())).await
}

/// What is at one path of a tree.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Seen {
    Directory,
    File(Vec<u8>),
    Link(PathBuf),
}

/// What is at a path, its permission bits, and when it was last modified.
/// What a platform cannot restore is left out: permissions and directory
/// times on Windows, and a link's own time everywhere.
pub(crate) type Described = (Seen, u32, Option<std::time::SystemTime>);

/// Everything at and under `root`, by path relative to it.
pub(crate) fn listing(root: &Path) -> BTreeMap<PathBuf, Described> {
    fn describe(path: &Path) -> Described {
        let found = std::fs::symlink_metadata(path).unwrap();
        #[cfg(unix)]
        let mode = std::os::unix::fs::PermissionsExt::mode(&found.permissions()) & 0o777;
        #[cfg(not(unix))]
        let mode = 0;
        let modified = found.modified().ok();
        if found.is_symlink() {
            (Seen::Link(std::fs::read_link(path).unwrap()), 0, None)
        } else if found.is_dir() {
            (Seen::Directory, mode, modified.filter(|_| cfg!(unix)))
        } else {
            (Seen::File(std::fs::read(path).unwrap()), mode, modified)
        }
    }
    fn visit(root: &Path, path: &Path, found: &mut BTreeMap<PathBuf, Described>) {
        let described = describe(path);
        let is_directory = described.0 == Seen::Directory;
        found.insert(path.strip_prefix(root).unwrap().to_owned(), described);
        if is_directory {
            for entry in std::fs::read_dir(path).unwrap() {
                visit(root, &entry.unwrap().path(), found);
            }
        }
    }
    let mut found = BTreeMap::new();
    visit(root, root, &mut found);
    found
}

/// Asserts that two trees are the same: the same files with the same bytes,
/// the same directories, empty ones included, the same links, and the same
/// permissions and times wherever this platform keeps them.
pub(crate) fn assert_same_tree(expected: &Path, actual: &Path) {
    let (expected, actual) = (listing(expected), listing(actual));
    let paths = |tree: &BTreeMap<PathBuf, Described>| tree.keys().cloned().collect::<Vec<_>>();
    assert_eq!(
        paths(&expected),
        paths(&actual),
        "the trees hold different paths"
    );
    for (path, described) in &expected {
        assert_eq!(
            Some(described),
            actual.get(path),
            "{} differs",
            path.display()
        );
    }
}
