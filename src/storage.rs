pub fn save_chunk(dir: &str, chunk: &Chunk) -> Result<(), SyncError>;
// Save chunk to disk

pub fn load_chunk(dir: &str, index: u64) -> Result<Chunk, SyncError>;
// Load a chunk from disk
