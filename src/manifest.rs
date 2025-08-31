use crate::error::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;
#[derive(Serialize, Deserialize, Debug)]
pub struct Manifest {
    pub file_name: String,
    pub file_size: u64,
    pub chunk_size: usize,
    pub chunks: Vec<[u8; 32]>,
}

impl Manifest {
    pub fn from_chunks(
        file_name: String,
        chunk_size: usize,
        chunks: &[super::chunk::Chunk],
    ) -> Self {
        let file_size = chunks.iter().map(|c| c.data.len() as u64).sum();
        let hashes: Vec<[u8; 32]> = chunks.iter().map(|c| c.hash).collect();
        Manifest {
            file_name,
            file_size,
            chunk_size,
            chunks: hashes,
        }
    }
}

pub fn write_manifest(manifest: &Manifest, path: &Path) -> Result<()> {
    let data = serde_json::to_string_pretty(manifest)?;
    std::fs::write(path, data)?;
    Ok(())
}

pub fn read_manifest(path: &Path) -> Result<Manifest> {
    let data = std::fs::read_to_string(path)?;
    let manifest = serde_json::from_str(&data)?;
    Ok(manifest)
}
