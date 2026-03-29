use clap::{Parser, Subcommand};
use dialoguer::Confirm;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use p2rent::chunk::{self, Chunk};
use p2rent::crypto::load_or_create_keypair;
use p2rent::manifest::{self, Manifest};
use p2rent::net::protocol::Message;
use p2rent::net::quic::{self, Peer, QuicClient, QuicServer};
use p2rent::scanner;
use p2rent::storage;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Instant;

#[derive(Parser, Debug)]
#[command(
    name = "p2rent",
    version,
    about = "Mini BitTorrent-like sharing over QUIC"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    Serve {
        #[arg(long, default_value = "127.0.0.1:5000")]
        addr: String,
        #[arg(long, default_value = "chunks")]
        storage_dir: PathBuf,
    },
    Share {
        path: PathBuf,
        #[arg(long, default_value_t = 1024 * 1024)]
        chunk_size: usize,
        #[arg(long, default_value = "manifests")]
        manifest_dir: PathBuf,
        #[arg(long, default_value = "chunks")]
        storage_dir: PathBuf,
        #[arg(long, default_value_t = false)]
        parallel: bool,
    },
    Fetch {
        #[arg(long)]
        addr: String,
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        out: Option<PathBuf>,
        #[arg(long)]
        stem: Option<String>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Serve { addr, storage_dir } => {
            let keypair = load_or_create_keypair()?;
            let listen_addr: SocketAddr = addr.parse()?;
            let server = QuicServer::bind(listen_addr, keypair).await?;
            println!("Listening on {listen_addr}");

            loop {
                match server.accept_and_handshake().await {
                    Ok(peer) => {
                        println!("Accepted new peer: {}", peer.id);
                        let storage_dir = storage_dir.clone();
                        tokio::spawn(async move {
                            handle_peer(peer, storage_dir).await;
                        });
                    }
                    Err(e) => {
                        eprintln!("Failed to accept peer: {}", e);
                    }
                }
            }
        }
        Commands::Share {
            path,
            chunk_size,
            manifest_dir,
            storage_dir,
            parallel,
        } => {
            if path.is_dir() {
                let files = scanner::scan_directory(&path)?;
                if files.is_empty() {
                    println!("No files found in directory.");
                    return Ok(());
                }

                println!(
                    "Preparing to share {} files (chunk_size={} bytes)\nManifests -> {:?}\nChunks    -> {:?}",
                    files.len(),
                    chunk_size,
                    &manifest_dir,
                    &storage_dir
                );

                if !Confirm::new()
                    .with_prompt("Continue?")
                    .default(true)
                    .interact()?
                {
                    println!("Cancelled.");
                    return Ok(());
                }

                let started = Instant::now();
                let m = MultiProgress::new();
                let total_pb = m.add(ProgressBar::new(files.len() as u64));
                total_pb.set_style(
                    ProgressStyle::with_template(
                        "{spinner} [{elapsed_precise}] [{bar:40}] {pos}/{len} files",
                    )
                    .unwrap()
                    .progress_chars("=>-"),
                );

                let total_bytes = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
                let total_chunks = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));

                if parallel {
                    use rayon::prelude::*;
                    files.par_iter().for_each(|file| {
                        if let Err(e) =
                            share_one_file(file, chunk_size, &manifest_dir, &storage_dir, None)
                        {
                            eprintln!("Failed to share {:?}: {}", file, e);
                        } else if let Ok(meta) = std::fs::metadata(file) {
                            total_bytes.fetch_add(meta.len(), std::sync::atomic::Ordering::Relaxed);
                        }
                        total_pb.inc(1);
                    });
                } else {
                    for file in &files {
                        match share_one_file(
                            file,
                            chunk_size,
                            &manifest_dir,
                            &storage_dir,
                            Some(&m),
                        ) {
                            Ok(info) => {
                                total_bytes.fetch_add(
                                    info.file_size,
                                    std::sync::atomic::Ordering::Relaxed,
                                );
                                total_chunks.fetch_add(
                                    info.num_chunks as u64,
                                    std::sync::atomic::Ordering::Relaxed,
                                );
                            }
                            Err(e) => eprintln!("Failed to share {:?}: {}", file, e),
                        }
                        total_pb.inc(1);
                    }
                }

                total_pb.finish_with_message("Completed");
                let elapsed = started.elapsed();
                let tb = total_bytes.load(std::sync::atomic::Ordering::Relaxed);
                let tc = total_chunks.load(std::sync::atomic::Ordering::Relaxed);
                println!(
                    "\nShared directory report:\n- Files: {}\n- Total bytes: {}\n- Total chunks: {}\n- Elapsed: {:.2?}\n- Throughput: {:.2} MB/s",
                    files.len(),
                    tb,
                    tc,
                    elapsed,
                    (tb as f64 / 1_048_576.0) / elapsed.as_secs_f64()
                );
            } else if path.is_file() {
                if let Err(e) = share_one_file(&path, chunk_size, &manifest_dir, &storage_dir, None)
                {
                    eprintln!("Failed: {}", e);
                }
            } else {
                eprintln!("Path does not exist: {:?}", path);
            }
        }
        Commands::Fetch {
            addr,
            manifest,
            out,
            stem,
        } => {
            // Load manifest and prepare output
            let manifest_data = p2rent::manifest::read_manifest(&manifest)?;
            let out_path = out.unwrap_or_else(|| PathBuf::from(&manifest_data.file_name));
            let stem = stem.unwrap_or_else(|| {
                PathBuf::from(&manifest_data.file_name)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("file")
                    .to_string()
            });

            let keypair = load_or_create_keypair()?;
            let client = QuicClient::new().await?;
            let addr: SocketAddr = addr.parse()?;
            let peer = client.connect_and_handshake(addr, &keypair).await?;
            let total = manifest_data.chunks.len() as u64;

            let pb = ProgressBar::new(total);
            pb.set_style(
                ProgressStyle::with_template(
                    "{spinner} [{bar:40}] {pos}/{len} chunks {elapsed_precise}",
                )
                .unwrap()
                .progress_chars("=>-"),
            );

            let mut received: Vec<Option<Vec<u8>>> = vec![None; total as usize];

            for index in 0..total {
                let (mut send, mut recv) = peer.connection.open_bi().await?;
                let req = Message::RequestChunk {
                    stem: stem.clone(),
                    index,
                };
                quic::send_message(&mut send, &req).await?;
                if let Message::Chunk { index: idx, data } =
                    quic::receive_message(&mut recv).await?
                    && idx == index
                {
                    received[idx as usize] = Some(data);
                    pb.inc(1);
                }
            }
            pb.finish_with_message("downloaded");

            let mut chunks_vec: Vec<Chunk> = Vec::with_capacity(received.len());
            for (i, maybe) in received.into_iter().enumerate() {
                let data = maybe.ok_or_else(|| anyhow::anyhow!("missing chunk {i} from server"))?;
                let hash: [u8; 32] = blake3::hash(&data).into();
                let expected = manifest_data.chunks[i];
                anyhow::ensure!(
                    hash == expected,
                    "chunk {i} integrity check failed — hash mismatch"
                );
                chunks_vec.push(Chunk {
                    index: i as u64,
                    hash,
                    size: data.len(),
                    data,
                });
            }
            chunk::combine_chunks(&chunks_vec, &out_path)?;
            println!("Written {}", out_path.display());
        }
    }

    Ok(())
}

struct ShareInfo {
    file_size: u64,
    num_chunks: usize,
}

fn share_one_file(
    file: &Path,
    chunk_size: usize,
    manifest_dir: &Path,
    storage_dir: &Path,
    mp: Option<&MultiProgress>,
) -> p2rent::error::Result<ShareInfo> {
    let file_name = file
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    let stem = file
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(&file_name);

    let meta = std::fs::metadata(file)?;
    let approx_total_chunks = (meta.len() as usize).div_ceil(chunk_size) as u64;

    let pb_spinner = ProgressBar::new_spinner();
    pb_spinner.set_style(ProgressStyle::with_template("{spinner} chunking {msg}").unwrap());
    pb_spinner.set_message(file_name.clone());
    pb_spinner.enable_steady_tick(std::time::Duration::from_millis(100));

    let started = Instant::now();
    let chunks: Vec<Chunk> = chunk::split_file(file, chunk_size)?;
    pb_spinner.finish_and_clear();

    let file_out_dir = {
        let mut d = PathBuf::from(storage_dir);
        d.push(stem);
        d
    };
    std::fs::create_dir_all(&file_out_dir)?;

    let save_pb = if let Some(m) = mp {
        let bar = m.add(ProgressBar::new(chunks.len() as u64));
        bar.set_style(
            ProgressStyle::with_template("{spinner} [{bar:40}] {pos}/{len} chunks {msg}")
                .unwrap()
                .progress_chars("=>-"),
        );
        bar.set_message(file_name.clone());
        Some(bar)
    } else {
        let bar = ProgressBar::new(chunks.len() as u64);
        bar.set_style(
            ProgressStyle::with_template("{spinner} [{bar:40}] {pos}/{len} chunks {msg}")
                .unwrap()
                .progress_chars("=>-"),
        );
        bar.set_message(file_name.clone());
        Some(bar)
    };

    let dir_str = file_out_dir
        .to_str()
        .ok_or_else(|| p2rent::error::SyncError::Other("non-UTF-8 path".into()))?;
    for c in &chunks {
        storage::save_chunk(dir_str, c)?;
        if let Some(pb) = &save_pb {
            pb.inc(1);
        }
    }
    if let Some(pb) = &save_pb {
        pb.finish_with_message("saved");
    }

    let manifest = Manifest::from_chunks(file_name.clone(), chunk_size, &chunks);
    let mut out_path = PathBuf::from(manifest_dir);
    std::fs::create_dir_all(&out_path)?;
    out_path.push(format!("{}.manifest.json", stem));
    manifest::write_manifest(&manifest, &out_path)?;

    let elapsed = started.elapsed();
    println!(
        "\nShared file report:\n- File: {}\n- Size: {} bytes\n- Chunk size: {} bytes\n- Chunks: {} (approx announced: {})\n- Manifest: {:?}\n- Chunks dir: {:?}\n- Elapsed: {:.2?}\n- Throughput: {:.2} MB/s",
        file.display(),
        meta.len(),
        chunk_size,
        chunks.len(),
        approx_total_chunks,
        out_path,
        file_out_dir,
        elapsed,
        (meta.len() as f64 / 1_048_576.0) / elapsed.as_secs_f64()
    );

    Ok(ShareInfo {
        file_size: meta.len(),
        num_chunks: chunks.len(),
    })
}

async fn handle_peer(peer: Peer, storage_dir: PathBuf) {
    println!("Handling connection with {}", peer.id);
    while let Ok((mut send, mut recv)) = peer.connection.accept_bi().await {
        match quic::receive_message(&mut recv).await {
            Ok(Message::RequestChunk { stem, index }) => {
                let mut dir = storage_dir.clone();
                dir.push(stem);
                let dir_str = dir.to_str().unwrap_or(".");
                match storage::load_chunk(dir_str, index) {
                    Ok(ch) => {
                        let msg = Message::Chunk {
                            index: ch.index,
                            data: ch.data,
                        };
                        let _ = quic::send_message(&mut send, &msg).await;
                    }
                    Err(e) => {
                        let _ = quic::send_message(&mut send, &Message::Bye).await;
                        eprintln!("load_chunk error: {}", e);
                    }
                }
            }
            Ok(_) => {
                let _ = quic::send_message(&mut send, &Message::Bye).await;
            }
            Err(e) => {
                eprintln!("recv error: {}", e);
                break;
            }
        }
    }
}
