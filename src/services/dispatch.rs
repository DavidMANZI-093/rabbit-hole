use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use tokio::net::TcpListener;

use crate::services::cli::Cmd;
use crate::services::fetch::fetch;
use crate::services::serve::build_shared;
use crate::services::tunnel::{self, Tunnel};
use crate::utils::log::{self, check_fail, check_ok};
use crate::utils::net::{lan_ip, shutdown_signal};
use crate::utils::progress::{FetchProgress, IngestProgress};
use crate::utils::ui::{
    dlabel, print_fetch_summary, print_ingest_summary, print_manifest_line, print_wan_and_fetch,
    tunnel_spinner,
};
use crate::{
    edge,
    ingest::{IngestStats, ingest},
};
use crate::{edge::DEFUALT_TTL_SECS, protocol::manifest::Manifest};
use crate::{info, warn};

pub async fn run(cmd: Cmd) -> Result<(), String> {
    match cmd {
        Cmd::Serve {
            path,
            block_size,
            secure,
            edge,
            no_color,
            verbose,
        } => {
            log::init_color(no_color);
            run_serve(ServeOpts {
                path,
                block_size,
                secure,
                edge,
                verbose,
            })
            .await
        }
        Cmd::Fetch {
            code_or_url,
            dest,
            timeout,
            bearer,
            force,
            concurrency,
            edge,
            no_color,
            verbose,
        } => {
            log::init_color(no_color);
            run_fetch(FetchOpts {
                code_or_url,
                dest,
                timeout,
                bearer,
                force,
                concurrency,
                edge,
                verbose,
            })
            .await
        }
        Cmd::Check { no_color } => {
            log::init_color(no_color);
            run_check().await
        }
    }
}

// ---------- serve ----------

async fn run_serve(opts: ServeOpts) -> Result<(), String> {
    log::set_verbose(opts.verbose);

    let progress = IngestProgress::new();
    let (manifest, stats) = ingest_blocking(&opts.path, opts.block_size, progress.clone()).await?;
    progress.finish_and_clear();

    print_ingest_summary(&stats);

    let token = opts.secure.then(crate::services::serve::generate_token);
    let shared = build_shared(&manifest, &opts.path, token.clone())?;

    print_manifest_line(shared.manifest_bytes.len() as u64);

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

    eprintln!("  {}  {}", dlabel("LAN"), crate::utils::log::bold(&lan_url));

    let (wan_url, tunnel, code) = start_tunnel(port, &opts.edge, &opts.path).await;

    print_wan_and_fetch(
        &lan_url,
        wan_url.as_deref(),
        code.as_deref(),
        token.as_deref(),
    );

    let mut server_handle = server_handle;
    tokio::select! {
        _ = shutdown_signal() => eprintln!("\n  signal: shutting down"),
        r = &mut server_handle => match r {
            Ok(Ok(())) => {},
            Ok(Err(e)) => eprintln!("  server error: {e}"),
            Err(e) => eprintln!("  server task panicked: {e}"),
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
    eprintln!("  bye.");

    Ok(())
}

// Spawns cloudflared and, if an edge base is configured, registers a short code.
async fn start_tunnel(
    port: u16,
    edge_base: &str,
    serve_path: &Path,
) -> (Option<String>, Option<Tunnel>, Option<String>) {
    let spinner = tunnel_spinner();
    spinner.set_message("requesting...");

    let (url, tunnel) = match tunnel::spawn(port).await {
        Ok(t) => {
            t.watch_exit();
            let url_clone = t.url.clone();
            let live = t
                .wait_until_live(|msg| {
                    spinner.set_message(format!("{url_clone}  {msg}"));
                })
                .await;
            spinner.finish_and_clear();
            if !live {
                warn!("tunnel URL not yet reachable at edge — sharing anyway");
            }
            (Some(t.url.clone()), Some(t))
        }
        Err(e) => {
            spinner.finish_and_clear();
            warn!("cloudflared not found ({e}) — LAN only (run: rh check)");
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
    log::set_verbose(opts.verbose);

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
    // Longest label is "cloudflared" (11 chars) → pad to 12 for alignment
    let lbl = |s: &str| crate::utils::log::dim(&format!("{s:<12}"));

    // cloudflared (resolved binary: private prefix, exe dir, or PATH)
    match std::process::Command::new(crate::cloudflared::binary())
        .arg("--version")
        .output()
    {
        Ok(out) if out.status.success() => {
            let raw = String::from_utf8_lossy(&out.stdout);
            let ver = raw.lines().next().unwrap_or("?").trim().to_string();
            let verdict =
                match crate::cloudflared::classify(crate::cloudflared::parse_version(&raw)) {
                    crate::cloudflared::Verdict::Supported(_) => "supported".to_string(),
                    crate::cloudflared::Verdict::TooOld(_) => {
                        format!("too old (need >= {})", crate::cloudflared::MIN)
                    }
                    crate::cloudflared::Verdict::Unknown => "version unknown".to_string(),
                };
            let src = if crate::cloudflared::is_bundled() {
                "bundled"
            } else {
                "PATH"
            };
            eprintln!(
                "  {}  {}   {}   {} ({})",
                lbl("cloudflared"),
                ver,
                check_ok(),
                verdict,
                src
            );
        }
        _ => {
            eprintln!(
                "  {}  not in PATH   {}   (run: sudo apt install cloudflared)",
                lbl("cloudflared"),
                check_fail()
            );
        }
    }

    // temp dir write
    let mut probe = std::env::temp_dir();
    probe.push(format!("rh-check-{}", std::process::id()));
    match std::fs::write(&probe, b"ok").and_then(|_| std::fs::remove_file(&probe)) {
        Ok(()) => eprintln!(
            "  {}  {}   {}",
            lbl("tmp write"),
            probe
                .parent()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            check_ok()
        ),
        Err(e) => eprintln!("  {}  {e}   {}", lbl("tmp write"), check_fail()),
    }

    Ok(())
}

// ---------- structs ----------

struct ServeOpts {
    path: PathBuf,
    block_size: u32,
    secure: bool,
    edge: String,
    verbose: bool,
}

struct FetchOpts {
    code_or_url: String,
    dest: PathBuf,
    timeout: u32,
    bearer: Option<String>,
    force: bool,
    concurrency: u8,
    edge: String,
    verbose: bool,
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
