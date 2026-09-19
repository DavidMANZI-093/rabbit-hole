use std::path::{Path, PathBuf};

use crate::info;
use crate::ingest::{IngestStats, ingest};
use crate::protocol::manifest::Manifest;
use crate::services::cli::Cmd;
use crate::services::serve::build_shared;
use crate::utils::log;
use crate::utils::progress::{IngestProgress, format_bytes};

pub async fn run(cmd: Cmd) -> Result<(), String> {
    match cmd {
        Cmd::Share {
            path,
            block_size,
            secure,
            verbose,
        } => {
            run_share(ShareOpts {
                path,
                block_size,
                secure,
                verbose,
            })
            .await
        }
        Cmd::Fetch { url, dest, verbose } => run_fetch(FetchOpts { url, dest, verbose }),
    }
}

async fn run_share(opts: ShareOpts) -> Result<(), String> {
    log::set_verbose(opts.verbose);
    info!("sharing content(s) at: {}", opts.path.display());

    let progress = IngestProgress::new();
    let (manifest, stats) = ingest_blocking(&opts.path, opts.block_size, progress.clone()).await?;
    progress.finish_and_clear();
    info!(
        "total files {} | {} | blocks {} ({} unique, {:.1}% dedup) | {} skipped",
        stats.files,
        format_bytes(stats.bytes),
        stats.blocks_total,
        stats.blocks_unique,
        if stats.blocks_total > 0 {
            (stats.blocks_total - stats.blocks_unique as u64) as f64 / stats.blocks_total as f64
                * 100.0
        } else {
            0.0
        },
        stats.skipped,
    );

    let token = opts.secure.then(crate::services::serve::generate_token);
    let shared = build_shared(&manifest, &opts.path, token.clone())?;

    Ok(())
}

fn run_fetch(opts: FetchOpts) -> Result<(), String> {
    log::set_verbose(opts.verbose);

    info!("fetching content(s) from: {}", opts.url);
    Ok(())
}

struct ShareOpts {
    path: PathBuf,
    block_size: u32,
    secure: bool,
    verbose: bool,
}

struct FetchOpts {
    url: String,
    dest: PathBuf,
    verbose: bool,
}

async fn ingest_blocking(
    path: &Path,
    block_size: u32,
    progress: IngestProgress,
) -> Result<(Manifest, IngestStats), String> {
    let owned = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        ingest(&owned, block_size, &progress).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}
