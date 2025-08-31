use crate::error::Result;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

pub fn scan_directory(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();

    for entry in WalkDir::new(dir) {
        let entry = entry?;
        if entry.file_type().is_file() {
            files.push(entry.path().to_path_buf());
        }
    }

    Ok(files)
}
