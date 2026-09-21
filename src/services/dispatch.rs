use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::net::TcpListener;

use crate::ingest::{IngestStats, ingest};
use crate::protocol::manifest::Manifest;
use crate::services::cli::Cmd;
use crate::services::fetch::fetch;
use crate::services::serve::build_shared;
use crate::services::tunnel::{self, Tunnel};
use crate::utils::log;
use crate::utils::progress::{FetchProgress, IngestProgress, format_bytes};
use crate::{info, warn};

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
        Cmd::Check => run_check().await,
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

    let mut tunnel: Option<Tunnel> = None;
    info!("starting 'cloudflared' tunnel...");
    match tunnel::spawn(local.port()).await {
        Ok(t) => {
            t.watch_exit();
            info!("tunnel: {} - waiting for edge route", t.url);
            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .map_err(|e| format!("http client: {e}"))?;
            if t.wait_until_live(&client).await {
                info!("tunnel: live at edge");
            } else {
                warn!("tunnel URL not yet reachable at edge");
            }
            tunnel = Some(t);
        }
        Err(e) => {
            warn!("tunnel failed ({e}); serving LAN-only, NOT halting");
        }
    }

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
    if let Some(t) = tunnel {
        t.shutdown().await;
        info!("tunnel: stopped");
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

    let progress = FetchProgress::new(opts.concurrency);
    let result = fetch(
        &opts.url,
        &opts.dest,
        opts.timeout,
        opts.bearer,
        opts.force,
        opts.concurrency,
        &progress,
    )
    .await;
    progress.finish_and_clear();
    let stats = result?;

    info!(
        "total files {} ({})\n{:7}blocks {} ({} fetched, {} failed) | retries {}\n{:7}dest {}",
        stats.files,
        format_bytes(stats.bytes),
        "",
        stats.blocks,
        stats.blocks,
        stats.failed,
        stats.retries,
        "",
        opts.dest.display()
    );

    Ok(())
}

async fn run_check() -> Result<(), String> {
    match std::process::Command::new("cloudflared")
        .arg("--version")
        .output()
    {
        Ok(out) if out.status.success() => {
            let v = String::from_utf8_lossy(&out.stdout);
            info!("cloudflared: {}", v.lines().next().unwrap_or("?").trim());
        }
        _ => warn!("cloudflared: not in PATH (tunnel shares unavailable)"),
    }
    let mut probe = std::env::temp_dir();
    probe.push(format!("rh-check-{}", std::process::id()));
    match std::fs::write(&probe, b"ok").and_then(|_| std::fs::remove_file(&probe)) {
        Ok(()) => info!("fs: temp writes ok"),
        Err(e) => warn!("fs: temp write failed: {e}"),
    }
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
