use crate::chunk::Chunk;
use crate::error::{Result, SyncError};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

fn chunk_path(dir: &Path, index: u64) -> PathBuf {
    let mut path = dir.to_path_buf();
    path.push(format!("{:016}.chunk", index));
    path
}

pub fn save_chunk(dir: &str, chunk: &Chunk) -> Result<()> {
    let dir_path = Path::new(dir);
    fs::create_dir_all(dir_path).map_err(SyncError::Io)?;
    let path = chunk_path(dir_path, chunk.index);
    let mut f = File::create(&path).map_err(SyncError::Io)?;
    f.write_all(&chunk.data).map_err(SyncError::Io)?;
    Ok(())
}

pub fn load_chunk(dir: &str, index: u64) -> Result<Chunk> {
    let dir_path = Path::new(dir);
    let path = chunk_path(dir_path, index);
    let mut f = File::open(&path).map_err(SyncError::Io)?;
    let mut data = Vec::new();
    f.read_to_end(&mut data).map_err(SyncError::Io)?;
    let hash: [u8; 32] = blake3::hash(&data).into();
    Ok(Chunk {
        index,
        hash,
        size: data.len(),
        data,
    })
}
