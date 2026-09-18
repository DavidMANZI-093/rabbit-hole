use std::path::PathBuf;

use crate::info;
use crate::ingest::{IngestStats, ingest};
use crate::protocol::manifest::Manifest;
use crate::services::cli::Cmd;
use crate::utils::log;

pub async fn run(cmd: Cmd) -> Result<(), String> {
    match cmd {
        Cmd::Share {
            path,
            block_size,
            verbose,
        } => {
            run_share(ShareOpts {
                path,
                block_size,
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

    let _ = ingest_blocking(opts.path, opts.block_size).await?;

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
    verbose: bool,
}

struct FetchOpts {
    url: String,
    dest: PathBuf,
    verbose: bool,
}

async fn ingest_blocking(
    path: PathBuf,
    block_size: u32,
) -> Result<(Manifest, IngestStats), String> {
    tokio::task::spawn_blocking(move || {
        ingest(&path.to_owned(), block_size).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}
