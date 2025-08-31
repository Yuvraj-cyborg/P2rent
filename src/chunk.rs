use crate::error::Result;
use std::{
    fs::File,
    io::{BufReader, Read, Write},
    path::Path,
};
pub struct Chunk {
    pub index: u64,
    pub hash: [u8; 32],
    pub data: Vec<u8>,
    pub size: usize,
}

pub fn split_file(path: &Path, chunk_size: usize) -> Result<Vec<Chunk>> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);

    let mut chunks = Vec::new();
    let mut buffer = vec![0u8; chunk_size];
    let mut index = 0;

    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        let data = buffer[..n].to_vec();
        let hash: [u8; 32] = blake3::hash(&data).into();
        chunks.push(Chunk {
            index,
            hash,
            data,
            size: n,
        });
        index += 1;
    }

    Ok(chunks)
}

pub fn combine_chunks(chunks: &[Chunk], output: &Path) -> Result<()> {
    let mut file = File::create(output)?;
    for chunk in chunks {
        file.write_all(&chunk.data)?;
    }
    Ok(())
}
