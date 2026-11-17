//! A store over a local folder, driven from plain test functions.
//!
//! The engine's store is asynchronous. A test of what ends up on disk reads
//! better one step after another, so this waits for each step.

use fs::Folder;
use model::artifact::{Artifact, Bitfield};
use model::place::Failure;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use tokio::runtime::Runtime;

static RUNTIME: LazyLock<Runtime> =
    LazyLock::new(|| Runtime::new().expect("the test runtime starts"));

/// Waits for `work` on the runtime the tests share.
pub(crate) fn wait<T>(work: impl Future<Output = T>) -> T {
    RUNTIME.block_on(work)
}

#[derive(Debug)]
pub(crate) struct Store {
    store: Arc<engine::Store>,
    folder: Folder,
}

impl Store {
    /// Over files that are already whole, as after indexing them.
    pub(crate) fn whole(artifact: Arc<Artifact>, root: PathBuf) -> Self {
        let folder = Folder::whole(artifact.clone(), root);
        let store = engine::Store::whole(artifact, Arc::new(folder.clone()));
        Self { store, folder }
    }

    /// Opens the artifact inside `dir`, keeping what is already there.
    pub(crate) fn resume(artifact: Arc<Artifact>, dir: &Path) -> Result<Self, Failure> {
        let folder = Folder::inside(artifact.clone(), dir);
        let place = Arc::new(folder.clone());
        let store = wait(engine::Store::resume(artifact, place, |_| {}))?;
        Ok(Self { store, folder })
    }

    pub(crate) fn root(&self) -> &Path {
        self.folder.root()
    }

    pub(crate) fn skipped_links(&self) -> usize {
        self.folder.skipped_links()
    }

    pub(crate) fn has(&self, index: u64) -> bool {
        self.store.has(index)
    }

    pub(crate) fn held(&self) -> Bitfield {
        self.store.held()
    }

    pub(crate) fn is_complete(&self) -> bool {
        self.store.is_complete()
    }

    pub(crate) fn read_chunk(&self, index: u64) -> Result<Vec<u8>, Failure> {
        wait(self.store.read_chunk(index))
    }

    pub(crate) fn write_chunk(&self, index: u64, data: &[u8]) -> Result<bool, Failure> {
        let put = wait(self.store.write_chunk(index, data.to_vec()))?;
        Ok(put != engine::Put::Refused)
    }

    pub(crate) fn seal(&self) -> Result<(), Failure> {
        wait(self.store.finish())
    }
}
