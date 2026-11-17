//! A local folder does what every place must.

use crate::common::wait;
use engine::conformance::{Sample, check, check_source};
use fs::Folder;
use std::sync::Arc;

#[test]
fn a_folder_is_a_place() {
    let tmp = tempfile::tempdir().unwrap();
    let checked = wait(check(|tree, name| {
        Arc::new(Folder::inside(tree.clone(), &tmp.path().join(name)))
    }));
    assert_eq!(checked, Ok(()));
}

#[test]
fn a_folder_is_a_source() {
    let tmp = tempfile::tempdir().unwrap();
    let sample = Sample::new().unwrap();
    let root = tmp.path().join("sample");
    std::fs::create_dir_all(root.join("sub")).unwrap();
    for (entry, bytes) in sample.artifact.entries().iter().zip(&sample.files) {
        if entry.is_file() {
            std::fs::write(root.join(&entry.path), bytes).unwrap();
        }
    }
    let folder = Folder::whole(sample.artifact.tree().clone(), root);
    assert_eq!(wait(check_source(&folder, &sample)), Ok(()));
}
