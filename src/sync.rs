use crate::{
    chunk::{self, Chunk},
    error::{Result, SyncError},
    manifest::{self, Manifest},
    scanner,
};
use std::fs;
use std::path::{Path, PathBuf};

pub fn sync_directory(
    root: &Path,
    manifest_dir: &Path,
    chunk_size: usize,
) -> Result<Vec<Manifest>> {
    fs::create_dir_all(manifest_dir).map_err(SyncError::Io)?;

    let files = scanner::scan_directory(root)?;
    let mut manifests = Vec::new();

    for file in files {
        let chunks: Vec<Chunk> = chunk::split_file(&file, chunk_size)
            .map_err(|e| SyncError::Other(format!("Failed to chunk file {:?}: {}", file, e)))?;

        let file_name = file
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        let stem = file
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(&file_name);

        let manifest = Manifest::from_chunks(file_name.clone(), chunk_size, &chunks);

        let mut out_path = PathBuf::from(manifest_dir);
        out_path.push(format!("{}.manifest.json", stem));

        manifest::write_manifest(&manifest, &out_path)?;

        manifests.push(manifest);
    }

    Ok(manifests)
}
