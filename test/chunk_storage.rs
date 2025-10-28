use p2rent::chunk::{combine_chunks, split_file, Chunk};
use p2rent::storage;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

#[test]
fn chunk_split_and_combine() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("data.bin");
    let mut f = fs::File::create(&file_path).unwrap();
    let data = (0..1024 * 3 + 123).map(|i| (i % 256) as u8).collect::<Vec<_>>();
    f.write_all(&data).unwrap();

    let chunks = split_file(&file_path, 1024).unwrap();
    assert!(chunks.len() >= 4);

    let out_path = dir.path().join("out.bin");
    combine_chunks(&chunks, &out_path).unwrap();
    let out_data = fs::read(&out_path).unwrap();
    assert_eq!(data, out_data);
}

#[test]
fn storage_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let chunk = Chunk { index: 0, hash: blake3::hash(b"hello").into(), data: b"hello".to_vec(), size: 5 };
    storage::save_chunk(dir.path().to_str().unwrap(), &chunk).unwrap();
    let loaded = storage::load_chunk(dir.path().to_str().unwrap(), 0).unwrap();
    assert_eq!(loaded.data, b"hello");
}

