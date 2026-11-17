//! A local folder does what every place must.

use crate::common::wait;
use fs::Folder;
use std::sync::Arc;

#[test]
fn a_folder_is_a_place() {
    let tmp = tempfile::tempdir().unwrap();
    let checked = wait(engine::conformance::check(|artifact, name| {
        Arc::new(Folder::inside(artifact.clone(), &tmp.path().join(name)))
    }));
    assert_eq!(checked, Ok(()));
}
