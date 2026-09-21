use std::path::{Path, PathBuf};

use tokio::net::TcpListener;

use crate::info;
use crate::ingest::{IngestStats, ingest};
use crate::protocol::manifest::Manifest;
use crate::services::cli::Cmd;
use crate::services::fetch::fetch;
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
        Cmd::Fetch {
            url,
            dest,
            timeout,
            bearer,
            force,
            concurrency,
            verbose,
        } => {
            run_fetch(FetchOpts {
                url,
                dest,
                timeout,
                bearer,
                force,
                concurrency,
                verbose,
            })
            .await
        }
    }
}

async fn run_share(opts: ShareOpts) -> Result<(), String> {
    log::set_verbose(opts.verbose);
    info!("sharing file(s) at: {}", opts.path.display());

    let progress = IngestProgress::new();
    let (manifest, stats) = ingest_blocking(&opts.path, opts.block_size, progress.clone()).await?;
    progress.finish_and_clear();
    info!(
        "total files {} ({}) \n{pad:>7}blocks {} ({} unique, {:.1}% dedup) \n{pad:>7}{} skipped",
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
        pad = "",
    );

    let token = opts.secure.then(crate::services::serve::generate_token);
    let shared = build_shared(&manifest, &opts.path, token.clone())?;

    info!(
        "manifest {}",
        format_bytes(shared.manifest_bytes.len() as u64)
    );

    let app = crate::services::serve::router(shared);
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|e| format!("bind 127.0.0.1:{}: {e}", 0))?;
    let local = listener
        .local_addr()
        .map_err(|e| format!("local addr: {e}"))?;

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    let mut server_handle = tokio::spawn(async move {
        info!("serving on http://{local}");
        if let Some(t) = &token {
            info!("token {t}");
        };
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                shutdown_rx.await.ok();
            })
            .await
    });

    tokio::select! {
        _ = shutdown_signal() => eprintln!("signal: shutting down ..."),
        r = &mut server_handle => match r {
            Ok(Ok(())) => eprintln!("server exited cleanly"),
            Ok(Err(e)) => eprintln!("server error: {e}"),
            Err(e) => eprintln!("server task panicked: {e}"),
        },
    }

    let _ = shutdown_tx.send(());
    tokio::time::timeout(std::time::Duration::from_secs(5), &mut server_handle)
        .await
        .ok();
    if !server_handle.is_finished() {
        server_handle.abort();
    }
    info!("bye.");

    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = signal(SignalKind::terminate()).expect("install SIGTERM handler");
        tokio::select! {
            _ = ctrl_c => {},
            _ = term.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        ctrl_c.await.ok();
    }
}

async fn run_fetch(opts: FetchOpts) -> Result<(), String> {
    log::set_verbose(opts.verbose);

    info!("fetching file(s) from: {}", opts.url);

    let stats = fetch(
        &opts.url,
        &opts.dest,
        opts.timeout,
        opts.bearer,
        opts.force,
        opts.concurrency,
    )
    .await?;

    info!(
        "done: {} files, {} bytes, {} blocks, {}",
        stats.files,
        stats.bytes,
        stats.blocks,
        opts.dest.display()
    );

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
    timeout: u32,
    bearer: Option<String>,
    force: bool,
    concurrency: u8,
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
