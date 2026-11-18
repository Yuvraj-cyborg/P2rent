//! Some of what a source holds, as a source of its own.
//!
//! A sync reads only what has changed. It names the entries it wants, and
//! copies from this in place of the whole source.

use model::place::{Capabilities, Description, Failure, Pending, Reader, Source};
use std::sync::Arc;

/// The entries of `source` at `entries`: their places among the entries
/// the source describes, in rising order.
///
/// What is picked has to be a tree of its own: the root, and with each
/// entry every directory it lies in. A copy from a part that is not one is
/// refused, as a tree that cannot be stored.
pub fn part(source: Arc<dyn Source>, entries: Vec<usize>) -> Arc<dyn Source> {
    Arc::new(Part { source, entries })
}

#[derive(Debug)]
struct Part {
    source: Arc<dyn Source>,
    /// For each entry of this part, its place in the source.
    entries: Vec<usize>,
}

impl Part {
    /// The place in the source of entry `file` of this part.
    fn place_of(&self, file: usize) -> Result<usize, Failure> {
        let place = self.entries.get(file).copied();
        place.ok_or_else(|| Failure::Absent(format!("there is no file {file}")))
    }
}

impl Source for Part {
    fn capabilities(&self) -> Capabilities {
        self.source.capabilities()
    }

    fn describe(&self) -> Pending<'_, Description> {
        Box::pin(async move {
            let whole = self.source.describe().await?;
            let missing =
                |place: &usize| Failure::Fatal(format!("the source has no entry {place} to pick"));
            let mut picked = Description {
                name: whole.name,
                entries: Vec::with_capacity(self.entries.len()),
                known: Vec::with_capacity(self.entries.len()),
            };
            for place in &self.entries {
                let entry = whole.entries.get(*place).ok_or_else(|| missing(place))?;
                let known = whole.known.get(*place).ok_or_else(|| missing(place))?;
                picked.entries.push(entry.clone());
                picked.known.push(known.clone());
            }
            Ok(picked)
        })
    }

    fn open(&self, file: usize, offset: u64, len: u64) -> Pending<'_, Box<dyn Reader>> {
        match self.place_of(file) {
            Ok(place) => self.source.open(place, offset, len),
            Err(failure) => Box::pin(std::future::ready(Err(failure))),
        }
    }

    fn read(&self, file: usize, offset: u64, len: u32) -> Pending<'_, Vec<u8>> {
        match self.place_of(file) {
            Ok(place) => self.source.read(place, offset, len),
            Err(failure) => Box::pin(std::future::ready(Err(failure))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conformance::{Memory, Sample};
    use crate::{Error, Options, copy};
    use model::artifact::MIN_CHUNK_SIZE;
    use model::place::Arrived;

    /// Copies the entries `picked` of the sample into a new place.
    fn copy_part(
        sample: &Sample,
        picked: Vec<usize>,
    ) -> (Result<crate::Copied, Error>, Option<Memory>) {
        let tree = sample.artifact.tree().clone();
        let whole = Arc::new(Memory::holding(tree, &sample.files).unwrap());
        let made = Arc::new(std::sync::Mutex::new(None));
        let open = {
            let made = made.clone();
            move |tree: &Arc<model::artifact::Tree>| {
                let place = Memory::new(tree.clone());
                *made.lock().unwrap() = Some(place.clone());
                Arc::new(place)
            }
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .start_paused(true)
            .build()
            .unwrap();
        let copied = runtime.block_on(copy(
            vec![part(whole, picked)],
            None,
            MIN_CHUNK_SIZE,
            open,
            Options::default(),
            Arc::new(|_| {}),
        ));
        let place = made.lock().unwrap().clone();
        (copied, place)
    }

    /// The sample is "", a.bin, empty, sub, sub/b.bin, sub/c.
    #[test]
    fn a_part_of_a_source_is_copied_and_nothing_else() {
        let sample = Sample::new().unwrap();
        let (copied, place) = copy_part(&sample, vec![0, 1, 3, 5]);
        let copied = copied.unwrap();
        let place = place.unwrap();

        let paths: Vec<&str> = copied
            .tree
            .entries()
            .iter()
            .map(|e| e.path.as_str())
            .collect();
        assert_eq!(paths, ["", "a.bin", "sub", "sub/c"]);
        assert_eq!(copied.tree.name(), "sample");
        // Each file of the part holds what the source has at its place.
        assert_eq!(place.committed(1).as_ref(), Some(&sample.files[1]));
        assert_eq!(place.committed(3).as_ref(), Some(&sample.files[5]));
        let bytes = (sample.files[1].len() + sample.files[5].len()) as u64;
        assert_eq!(copied.fetched, bytes);
    }

    #[test]
    fn a_part_that_is_not_a_tree_or_names_what_is_not_there_is_refused() {
        let sample = Sample::new().unwrap();
        // A file without the directory it lies in.
        let (copied, _) = copy_part(&sample, vec![0, 5]);
        assert!(
            matches!(copied, Err(Error::Place(Failure::Corrupt(_)))),
            "{copied:?}"
        );
        // An entry the source does not have.
        let (copied, _) = copy_part(&sample, vec![0, 6]);
        assert!(
            matches!(copied, Err(Error::Place(Failure::Fatal(_)))),
            "{copied:?}"
        );
    }

    #[test]
    fn what_a_part_does_not_have_cannot_be_opened() {
        let sample = Sample::new().unwrap();
        let tree = sample.artifact.tree().clone();
        let whole = Arc::new(Memory::holding(tree, &sample.files).unwrap());
        let picked = part(whole, vec![0, 5]);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        // Entry 1 of the part is the source's sub/c, one byte.
        let read = runtime.block_on(picked.read(1, 0, 1));
        assert_eq!(read.as_deref(), Ok(&sample.files[5][..]));
        let mut reader = runtime.block_on(picked.open(1, 0, 1)).unwrap();
        assert_eq!(
            runtime
                .block_on(reader.read(1, &Arrived::default()))
                .as_deref(),
            Ok(&sample.files[5][..])
        );
        for missing in [2, 9] {
            assert!(matches!(
                runtime.block_on(picked.read(missing, 0, 1)),
                Err(Failure::Absent(_))
            ));
            assert!(matches!(
                runtime.block_on(picked.open(missing, 0, 1)),
                Err(Failure::Absent(_))
            ));
        }
    }
}
