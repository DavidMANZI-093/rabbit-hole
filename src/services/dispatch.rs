use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use tokio::net::TcpListener;

use crate::error;
use crate::report::{print_fetch_summary, print_serve_details};
use crate::utils::net::{lan_ip, shutdown_signal};
use crate::utils::progress::{FetchProgress, IngestProgress};
use crate::{
    edge,
    ingest::{IngestStats, ingest},
};
use crate::{edge::DEFUALT_TTL_SECS, protocol::manifest::Manifest};
use crate::{info, warn};
use crate::{services::cli::Command, utils::format::format_bytes};
use crate::{services::fetch::fetch, utils::tty::yellow};
use crate::{
    services::serve::build_shared,
    utils::tty::{green, purple, red},
};
use crate::{
    services::tunnel::{self, Tunnel},
    utils::tty::{Dots, bold},
};

pub async fn run(cmd: Command) -> Result<(), String> {
    match cmd {
        Command::Serve {
            path,
            block_size,
            secure,
            local,
            edge,
        } => {
            run_serve(ServeOpts {
                path,
                block_size,
                secure,
                local,
                edge,
            })
            .await
        }
        Command::Fetch {
            code_or_url,
            dest,
            timeout,
            bearer,
            force,
            concurrency,
            edge,
        } => {
            // log::init_color(no_color);
            run_fetch(FetchOpts {
                code_or_url,
                dest,
                timeout,
                bearer,
                force,
                concurrency,
                edge,
            })
            .await
        }
        Command::Check => run_check().await,
    }
}

// ---------- serve ----------

async fn run_serve(opts: ServeOpts) -> Result<(), String> {
    let progress = IngestProgress::new(&opts.path);
    let (manifest, stats) = ingest_blocking(&opts.path, opts.block_size, progress.clone()).await?;
    progress.finish_and_clear();

    if stats.skipped > 0 {
        warn!(
            "skipped {} entries, rerun with '--verbose' for reasons",
            stats.skipped
        );
    }
    info!("{}", stats.summary());

    let token = opts.secure.then(crate::services::serve::generate_token);
    let shared = build_shared(&manifest, &opts.path, token.clone())?;

    info!(
        "manifest size {}\n",
        format_bytes(shared.manifest_bytes.len() as u64)
    );

    let app = crate::services::serve::router(shared.clone());

    let listener = TcpListener::bind(("0.0.0.0", 0u16))
        .await
        .map_err(|e| format!("bind: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("local addr: {e}"))?
        .port();

    let lan_url = format!("http://{}:{port}", lan_ip());

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    let server_handle = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                shutdown_rx.await.ok();
            })
            .await
    });

    let (wan_url, tunnel, code) = start_tunnel(port, &opts.edge, &opts.path).await;

    print_serve_details(
        &lan_url,
        wan_url.as_deref(),
        code.as_deref(),
        token.as_deref(),
        opts.local,
    );

    let mut server_handle = server_handle;
    tokio::select! {
        _ = shutdown_signal() => (),
        r = &mut server_handle => match r {
            Ok(Ok(())) => {},
            Ok(Err(e)) => error!("server error: {e}"),
            Err(e) => error!("server task panicked: {e}"),
        },
    }

    let _ = shutdown_tx.send(());
    tokio::time::timeout(Duration::from_secs(5), &mut server_handle)
        .await
        .ok();
    if !server_handle.is_finished() {
        server_handle.abort();
    }
    if let Some(t) = tunnel {
        t.shutdown().await;
    }

    Ok(())
}

// Spawns cloudflared and, if an edge base is configured, registers a short code.
async fn start_tunnel(
    port: u16,
    edge_base: &str,
    serve_path: &Path,
) -> (Option<String>, Option<Tunnel>, Option<String>) {
    let status = Dots::start("requesting tunnel");

    let (url, tunnel) = match tunnel::spawn(port).await {
        Ok(t) => {
            t.watch_exit();
            status.message("resolving dns");
            let live = t.wait_until_live(|_| {}).await;
            status.finish();
            if !live {
                warn!("tunnel URL not yet reachable at edge — sharing anyway");
            }
            (Some(t.url.clone()), Some(t))
        }
        Err(tunnel::SpawnError::NotInstalled(e)) => {
            status.finish();
            warn!("cloudflared not found ({e}) — LAN only (run: rh check)");
            (None, None)
        }
        Err(tunnel::SpawnError::NoTunnelUrl(e)) => {
            status.finish();
            warn!("no tunnel URL ({e}) — LAN only (check your internet connection)");
            (None, None)
        }
    };

    let code = match (url.as_deref(), edge_base.is_empty()) {
        (Some(url), false) => {
            let memory = edge::memory_path_for(serve_path);
            match edge::try_claim_remembered(edge_base, url, DEFUALT_TTL_SECS, &memory).await {
                Some(c) => {
                    info!("edge code  {c}  (ttl {DEFUALT_TTL_SECS}s)");
                    Some(c)
                }
                None => None,
            }
        }
        _ => None,
    };

    (url, tunnel, code)
}

// ---------- fetch ----------

async fn run_fetch(opts: FetchOpts) -> Result<(), String> {
    let url = resolve(&opts.code_or_url, &opts.edge).await?;
    info!("fetching from: {url}");

    let progress = FetchProgress::new(opts.concurrency);
    let result = fetch(
        &url,
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

    print_fetch_summary(
        stats.files,
        stats.bytes,
        stats.blocks,
        stats.failed,
        stats.retries,
        &opts.dest,
    );

    Ok(())
}

// Resolves `code_or_url` to a full HTTPS URL.
async fn resolve(code_or_url: &str, edge_base: &str) -> Result<String, String> {
    if code_or_url.starts_with("http://") || code_or_url.starts_with("https://") {
        return Ok(code_or_url.to_string());
    }
    if edge_base.is_empty() {
        return Err(format!(
            "{code_or_url:?} looks like a short code but no edge base is configured \
             (set RH_EDGE_BASE at build time or pass --edge <url>)"
        ));
    }
    edge::EdgeClient::new(edge_base)
        .map_err(|e| e.to_string())?
        .lookup(code_or_url)
        .await
        .map_err(|e| format!("edge lookup of {code_or_url:?}: {e}"))
}

// ---------- check ----------

async fn run_check() -> Result<(), String> {
    // cloudflared (resolved binary: private prefix, exe dir, or PATH)
    match std::process::Command::new(crate::cloudflared::binary())
        .arg("--version")
        .output()
    {
        Ok(out) if out.status.success() => {
            let raw = String::from_utf8_lossy(&out.stdout);

            // Based on the assumption that `cloudflared --version` returns the FIRST line in format,
            // `cloudflared version YYYY.M.P (built YYYYMMDD-HH:MM:SS)`.
            // (For YYYY.M.P, see `crate::cloudflared::CalVer`)
            let ver = raw.lines().next().unwrap_or("?").trim().to_string();

            let src = if crate::cloudflared::is_bundled() {
                "bundled"
            } else {
                "in PATH"
            };

            match crate::cloudflared::classify(crate::cloudflared::parse_version(&raw)) {
                crate::cloudflared::Verdict::Supported(_) => {
                    info!(
                        "cloudflared: {} ({src}) - {ver}",
                        bold(green("available").as_str())
                    );
                }
                crate::cloudflared::Verdict::TooOld(v) => {
                    info!(
                        "cloudflared: {}, need >= {} (have {v}) (see {})",
                        bold(yellow("too old").as_str()),
                        crate::cloudflared::MIN,
                        crate::cloudflared::SOURCE_PAGE
                    );
                }
                crate::cloudflared::Verdict::Unknown => {
                    info!(
                        "cloudflared: {} ({src}) - {ver}",
                        bold(purple("unknown").as_str())
                    );
                }
            }
        }
        _ => {
            info!(
                "cloudflared: {} (see: {})",
                bold(red("missing").as_str()),
                crate::cloudflared::SOURCE_PAGE
            );
        }
    }

    Ok(())
}

// ---------- structs ----------

struct ServeOpts {
    path: PathBuf,
    block_size: u32,
    secure: bool,
    local: bool,
    edge: String,
}

struct FetchOpts {
    code_or_url: String,
    dest: PathBuf,
    timeout: u32,
    bearer: Option<String>,
    force: bool,
    concurrency: u8,
    edge: String,
}

// ---------- helpers ----------

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
