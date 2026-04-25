use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

mod bfs;

use bfs::Bfs;

#[derive(Parser)]
#[command(name = "mkfs.bfs", version, about = "Create SCO BFS filesystem images")]
struct Cli {
    /// Populate from this directory (flattened — BFS has no subdirectories)
    #[arg(short = 'd', long)]
    directory: Option<PathBuf>,

    /// Output filename
    filename: PathBuf,

    /// Image size in megabytes
    size_mb: u32,
}

fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    if cli.size_mb == 0 {
        anyhow::bail!("size must be positive");
    }

    let mut bfs = Bfs::new(cli.size_mb)?;
    if let Some(src) = &cli.directory {
        bfs.populate_flat(src)?;
    }
    let stats = bfs.write_to(&cli.filename)?;

    println!(
        "Created BFS image: {} ({}MB, {} blocks, {} entries)",
        cli.filename.display(),
        cli.size_mb,
        stats.total_blocks,
        stats.entries,
    );
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("mkfs.bfs: {e:#}");
            ExitCode::FAILURE
        }
    }
}
