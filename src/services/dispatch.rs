use std::{
    net::{IpAddr, Ipv4Addr, UdpSocket},
    path::{Path, PathBuf},
    time::Duration,
};

use tokio::net::TcpListener;

use crate::ingest::{IngestStats, ingest};
use crate::protocol::manifest::Manifest;
use crate::services::cli::Cmd;
use crate::services::fetch::fetch;
use crate::services::serve::build_shared;
use crate::services::tunnel::{self, Tunnel};
use crate::utils::log::{self, bold, dim, yellow};
use crate::utils::progress::{FetchProgress, IngestProgress, format_bytes};
use crate::{info, warn};

pub async fn run(cmd: Cmd) -> Result<(), String> {
    match cmd {
        Cmd::Serve {
            path,
            block_size,
            secure,
            no_color,
            verbose,
        } => {
            log::init_color(no_color);
            run_serve(ServeOpts {
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
            no_color,
            verbose,
        } => {
            log::init_color(no_color);
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

async fn run_serve(opts: ServeOpts) -> Result<(), String> {
    log::set_verbose(opts.verbose);

    let progress = IngestProgress::new();
    let (manifest, stats) = ingest_blocking(&opts.path, opts.block_size, progress.clone()).await?;
    progress.finish_and_clear();

    print_ingest_summary(&stats);

    let token = opts.secure.then(crate::services::serve::generate_token);
    let shared = build_shared(&manifest, &opts.path, token.clone())?;

    let app = crate::services::serve::router(shared.clone());

    // bind on all interfaces so LAN receivers can connect
    let listener = TcpListener::bind(("0.0.0.0", 0u16))
        .await
        .map_err(|e| format!("bind: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("local addr: {e}"))?
        .port();

    let lan_ip = lan_ip();
    let lan_url = format!("http://{}:{port}", lan_ip);

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    let server_handle = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                shutdown_rx.await.ok();
            })
            .await
    });

    // LAN URL is available now — print it immediately so the user has something to copy
    // before the tunnel warms up (which can take several seconds)
    print_manifest_line(shared.manifest_bytes.len() as u64);
    eprintln!(
        "  {label:<8}  {url}",
        label = dim("LAN"),
        url = bold(&lan_url)
    );

    // start tunnel; print WAN URL once confirmed live
    let wan_url: Option<String>;
    let mut tunnel: Option<Tunnel> = None;

    match tunnel::spawn(port).await {
        Ok(t) => {
            t.watch_exit();
            let tunnel_url = t.url.clone();

            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .map_err(|e| format!("http client: {e}"))?;

            if t.wait_until_live(&client).await {
                wan_url = Some(tunnel_url);
            } else {
                warn!("tunnel URL not yet reachable at edge");
                wan_url = Some(t.url.clone());
            }
            tunnel = Some(t);
        }
        Err(e) => {
            warn!("cloudflared not found ({e}) — LAN only (run: rh check)");
            wan_url = None;
        }
    }

    // print WAN URL + the actionable fetch command
    print_urls_remaining(&lan_url, wan_url.as_deref(), token.as_deref());

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

// ---------- output helpers ----------

fn print_ingest_summary(stats: &IngestStats) {
    let dedup_pct = if stats.blocks_total > 0 {
        (stats.blocks_total - stats.blocks_unique as u64) as f64 / stats.blocks_total as f64 * 100.0
    } else {
        0.0
    };
    eprintln!(
        "  {files:<8}  {n}   {size}",
        files = dim("files"),
        n = stats.files,
        size = format_bytes(stats.bytes),
    );
    eprintln!(
        "  {blocks:<8}  {total}   {unique} unique   {dedup_pct:.1}% dedup",
        blocks = dim("blocks"),
        total = stats.blocks_total,
        unique = stats.blocks_unique,
    );
    if stats.skipped > 0 {
        eprintln!(
            "  {skipped:<8}  {} skipped",
            stats.skipped,
            skipped = yellow("warn"),
        );
    }
    eprintln!();
}

fn print_manifest_line(len: u64) {
    eprintln!(
        "  {label:<8}  {size}",
        label = dim("manifest"),
        size = format_bytes(len)
    );
    eprintln!();
}

fn print_urls_remaining(lan: &str, wan: Option<&str>, token: Option<&str>) {
    if let Some(wan_url) = wan {
        eprintln!(
            "  {label:<8}  {url}",
            label = dim("WAN"),
            url = bold(wan_url)
        );
    }
    let fetch_cmd = match (wan, token) {
        (Some(wan_url), Some(_)) => format!("rh fetch {wan_url} <dest> --bearer <token>"),
        (Some(wan_url), None) => format!("rh fetch {wan_url} <dest>"),
        (None, Some(_)) => format!("rh fetch {lan} <dest> --bearer <token>"),
        (None, None) => format!("rh fetch {lan} <dest>"),
    };
    if let Some(t) = token {
        eprintln!();
        eprintln!("  {label:<8}  {t}", label = dim("token"));
    }
    eprintln!();
    eprintln!("  {fetch_cmd}");
    eprintln!();
}

// probes the routing table — returns the IP that would be used to reach 8.8.8.8
// (no packet is actually sent; this is just a socket trick to read the kernel's choice)
fn lan_ip() -> IpAddr {
    UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| {
            s.connect("8.8.8.8:80")?;
            s.local_addr()
        })
        .map(|a| a.ip())
        .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST))
}

// ---------- shutdown ----------

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

// ---------- fetch ----------

async fn run_fetch(opts: FetchOpts) -> Result<(), String> {
    log::set_verbose(opts.verbose);

    info!("fetching from: {}", opts.url);

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

    eprintln!();
    eprintln!(
        "  {label:<8}  {n}   {size}",
        label = dim("files"),
        n = stats.files,
        size = format_bytes(stats.bytes),
    );
    eprintln!(
        "  {label:<8}  {n}   {failed} failed   {retries} retries",
        label = dim("blocks"),
        n = stats.blocks,
        failed = stats.failed,
        retries = stats.retries,
    );
    eprintln!("  {label:<8}  {}", opts.dest.display(), label = dim("dest"),);
    eprintln!();

    Ok(())
}

// ---------- check ----------

async fn run_check() -> Result<(), String> {
    match std::process::Command::new("cloudflared")
        .arg("--version")
        .output()
    {
        Ok(out) if out.status.success() => {
            let v = String::from_utf8_lossy(&out.stdout);
            info!("cloudflared  {}", v.lines().next().unwrap_or("?").trim());
        }
        _ => warn!("cloudflared: not in PATH — tunnel shares unavailable"),
    }
    let mut probe = std::env::temp_dir();
    probe.push(format!("rh-check-{}", std::process::id()));
    match std::fs::write(&probe, b"ok").and_then(|_| std::fs::remove_file(&probe)) {
        Ok(()) => info!("temp writes ok"),
        Err(e) => warn!("temp write failed: {e}"),
    }
    Ok(())
}

// ---------- helpers ----------

struct ServeOpts {
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
