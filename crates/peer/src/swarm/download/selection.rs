//! Download just the entries a sync needs. Suppliers still serve the ticket's
//! original chunk numbers and content id. Only the local store sees a smaller tree.

use super::{Found, Node, bytes_of, fetch_hints};
use engine::{Copied, Options, Store};
use fs::Folder;
use model::artifact::{Artifact, Bitfield, Manifest, Tree};
use model::place::{
    Arrived, Capabilities, Event, Failure, Observer, Pending, Status, Supplier, SupplierId, Supply,
};
use model::search::ChunkHints;
use std::sync::Arc;

impl Found {
    /// Fetches selected entries into the folder made by `open`. The selection
    /// is local to this transfer and is never advertised to peers. Entries
    /// must be in tree order and include each selected entry's parents.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid selection or if the transfer fails.
    pub async fn fetch_entries(
        &self,
        node: &Arc<Node>,
        entries: &[usize],
        name: &str,
        open: impl FnOnce(&Arc<Tree>) -> Arc<Folder>,
        options: Options,
        observer: Observer,
    ) -> Result<Copied, engine::Error> {
        let original = &self.artifact;
        let (artifact, selection) = Selection::pick(original, name, entries)?;
        let tree = artifact.tree().clone();
        observer(Event::Planned {
            bytes: tree.total_size(),
            files: tree
                .entries()
                .iter()
                .filter(|entry| entry.is_file())
                .count() as u64,
        });
        let folder = open(&tree);
        let scanning = observer.clone();
        let hints = async {
            let secret = self.access.secret;
            let all = fetch_hints(node, &self.manifest_peer, original, &secret).await?;
            match &selection {
                None => Some(all),
                Some(part) => Some(ChunkHints::pack(
                    part.chunks.iter().filter_map(|&i| all.get(i)),
                )),
            }
        };
        let store = Store::resume_with_hints(
            artifact.clone(),
            folder,
            move |bytes| scanning(Event::Examined { bytes }),
            hints,
        )
        .await?;
        let resumed = bytes_of(&artifact, &store.held());
        observer(Event::Kept { bytes: resumed });

        // What is fetched here is offered to nobody, so this node asks to
        // be listed nowhere.
        let (_, mut suppliers) = self.suppliers(node, false);
        if let Some(selection) = selection {
            suppliers = suppliers
                .into_iter()
                .map(|supplier| selection.supplier(supplier))
                .collect();
        }
        let ids: Vec<_> = suppliers.iter().map(|supplier| supplier.id()).collect();
        let summary = engine::transfer(
            &store,
            *node.node_id().as_bytes(),
            suppliers,
            options,
            observer,
        )
        .await?;
        store.finish().await?;
        let from = ids
            .iter()
            .map(|id| {
                summary
                    .sources
                    .iter()
                    .find(|(who, _)| who == id)
                    .map_or(0, |(_, n)| *n)
            })
            .collect();
        Ok(Copied {
            fetched: tree.total_size() - resumed,
            resumed,
            from,
            tree,
            artifact: Some(artifact),
            verification: Some(summary.verification),
        })
    }
}

#[derive(Debug)]
struct Selection {
    chunks: Vec<u64>,
    original_count: u64,
    tree: Arc<Tree>,
}

impl Selection {
    fn pick(
        original: &Arc<Artifact>,
        name: &str,
        entries: &[usize],
    ) -> Result<(Arc<Artifact>, Option<Arc<Self>>), Failure> {
        if name == original.name() && entries.iter().copied().eq(0..original.entries().len()) {
            return Ok((original.clone(), None));
        }
        let mut selected = Vec::with_capacity(entries.len());
        let mut chunks = Vec::new();
        let mut hashes = Vec::new();
        for &entry in entries {
            let (item, metadata) = original
                .entries()
                .get(entry)
                .zip(original.metadata(entry))
                .ok_or_else(|| Failure::Fatal(format!("no entry {entry} in the ticket")))?;
            selected.push((item.clone(), metadata));
            for index in original.file_chunks(entry) {
                hashes.push(original.chunk_hash(index).ok_or_else(|| {
                    Failure::Corrupt(format!("missing digest for chunk {index}"))
                })?);
                chunks.push(index);
            }
        }
        let manifest = Manifest::new(name, original.chunk_size(), selected, hashes);
        let artifact =
            Arc::new(Artifact::new(manifest).map_err(|e| Failure::Fatal(e.to_string()))?);
        // Whole files in manifest order: if no chunks were skipped, even a
        // renamed snapshot can use the original suppliers and hints directly.
        let part = (artifact.chunk_count() != original.chunk_count()).then(|| {
            Arc::new(Self {
                chunks,
                original_count: original.chunk_count(),
                tree: artifact.tree().clone(),
            })
        });
        Ok((artifact, part))
    }

    fn supplier(self: &Arc<Self>, inner: Arc<dyn Supplier>) -> Arc<dyn Supplier> {
        Arc::new(Selected {
            selection: self.clone(),
            inner,
        })
    }

    fn original(&self, index: u64) -> Option<u64> {
        self.chunks.get(usize::try_from(index).ok()?).copied()
    }

    fn held(&self, original: &Bitfield) -> Result<Bitfield, Failure> {
        if !original.is_well_formed(self.original_count) {
            return Err(Failure::Corrupt(
                "supplier described a different chunk layout".into(),
            ));
        }
        let mut held = Bitfield::new(self.tree.chunk_count());
        for (index, chunk) in self.chunks.iter().enumerate() {
            if original.get(*chunk) {
                held.set(index as u64);
            }
        }
        Ok(held)
    }

    // Within each selected file the two numberings are contiguous. Stop
    // read-ahead at its boundary in case the next selected file is far away.
    fn request(&self, index: u64, following: u64) -> Option<(u64, u64)> {
        let original = self.original(index)?;
        let chunk = self.tree.locate(index)?;
        let left = self.tree.file_chunks(chunk.file).end - index - 1;
        Some((original, following.min(left)))
    }
}

#[derive(Debug)]
struct Selected<S: ?Sized> {
    selection: Arc<Selection>,
    inner: Arc<S>,
}

impl Supplier for Selected<dyn Supplier> {
    fn id(&self) -> SupplierId {
        self.inner.id()
    }

    fn open(&self) -> Pending<'_, Arc<dyn Supply>> {
        Box::pin(async move {
            let link: Arc<dyn Supply> = Arc::new(Selected {
                selection: self.selection.clone(),
                inner: self.inner.open().await?,
            });
            Ok(link)
        })
    }
}

impl Supply for Selected<dyn Supply> {
    fn status(&self) -> Pending<'_, Status> {
        Box::pin(async move {
            let status = self.inner.status().await?;
            let held = self.selection.held(&status.held)?;
            let others = status
                .others
                .into_iter()
                .map(|s| self.selection.supplier(s))
                .collect();
            Ok(Status { held, others })
        })
    }

    fn chunk<'a>(
        &'a self,
        index: u64,
        following: u64,
        arrived: &'a Arrived,
    ) -> Pending<'a, Vec<u8>> {
        match self.selection.request(index, following) {
            Some((original, following)) => self.inner.chunk(original, following, arrived),
            None => Box::pin(std::future::ready(Err(Failure::Fatal(
                "selected chunk is out of range".into(),
            )))),
        }
    }

    fn leave(&self, index: u64) {
        if let Some(original) = self.selection.original(index) {
            self.inner.leave(original);
        }
    }

    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::artifact::{Entry, EntryKind, EntryMetadata, Hash, MIN_CHUNK_SIZE};

    /// Check all file subsets and all supplier bitfields for this six-chunk
    /// snapshot, including gaps, the empty selection and the whole artifact.
    #[test]
    fn selected_requests_and_availability_agree_with_the_original_manifest() {
        let files = [("a", 1u8), ("b", 2), ("c", 3)];
        let meta = EntryMetadata {
            mode: 0o755,
            modified: None,
        };
        let root = Entry {
            path: String::new(),
            kind: EntryKind::Directory,
        };
        let mut entries = vec![(root, meta)];
        for (path, chunks) in files {
            let size = u64::from(chunks) * u64::from(MIN_CHUNK_SIZE) - 1;
            entries.push((
                Entry {
                    path: path.into(),
                    kind: EntryKind::File { size },
                },
                meta,
            ));
        }
        let hashes = (0..6u8).map(|i| Hash::of(&[i])).collect();
        let manifest = Manifest::new("root", MIN_CHUNK_SIZE, entries, hashes);
        let full = Arc::new(Artifact::new(manifest).unwrap());
        let layout = [[0u64].as_slice(), &[1, 2], &[3, 4, 5]];
        for mask in 0..8u8 {
            let chosen: Vec<_> = (0..3).filter(|&file| mask & (1 << file) != 0).collect();
            let entries: Vec<_> = [0]
                .into_iter()
                .chain(chosen.iter().map(|i| i + 1))
                .collect();
            let (local, part) = Selection::pick(&full, "renamed", &entries).unwrap();
            let mut expected = Vec::new();
            for file in chosen {
                let chunks = layout[file];
                for (i, &original) in chunks.iter().enumerate() {
                    expected.push((chunks.len() - i - 1, original));
                }
            }
            assert_eq!(local.chunk_count(), expected.len() as u64);
            let Some(part) = part else {
                assert_eq!(mask, 7);
                continue;
            };
            for (local_index, &(left, original)) in expected.iter().enumerate() {
                let index = local_index as u64;
                assert_eq!(local.chunk_hash(index), full.chunk_hash(original));
                assert_eq!(part.request(index, u64::MAX), Some((original, left as u64)));
                assert_eq!(part.request(index, 0), Some((original, 0)));
            }
            assert_eq!(part.request(local.chunk_count(), 0), None);
            assert!(part.held(&Bitfield::full(7)).is_err());
            for advertised in 0..64u8 {
                let mut bits = Bitfield::new(6);
                for i in 0..6 {
                    if advertised & (1 << i) != 0 {
                        bits.set(i);
                    }
                }
                let held = part.held(&bits).unwrap();
                assert_eq!(held.len(), local.chunk_count());
                for (i, &(_, original)) in expected.iter().enumerate() {
                    assert_eq!(held.get(i as u64), bits.get(original));
                }
            }
        }
    }
}
