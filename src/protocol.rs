use serde::{Deserialize, Serialize};
#[derive(Serialize, Deserialize, Debug)]
pub enum Message {
    Handshake {
        file_hash: String,
        total_chunks: u64,
    },
    /// Client requests a single chunk by file stem and chunk index
    RequestChunk {
        stem: String,
        index: u64,
    },
    Have {
        chunks: Vec<u64>,
    },
    Need {
        chunks: Vec<u64>,
    },
    Chunk {
        index: u64,
        data: Vec<u8>,
    },
    Bye,
}
