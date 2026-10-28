use std::path::{Path, PathBuf};

use crate::error;
use crate::protocol::manifest::Manifest;
use crate::report::{print_fetch_summary, print_serve_details};
use crate::utils::net::lan_ip;
use crate::utils::progress::{FetchProgress, IngestProgress};
use crate::{
    edge,
    ingest::{IngestStats, ingest},
};
use crate::{info, warn};
use crate::{services::cli::Command, utils::format::format_bytes};
use crate::{services::fetch::fetch, utils::tty::yellow};
use crate::{
    services::share::{build_shared, generate_token},
    utils::tty::{green, purple, red},
};
use crate::{
    services::{serve::Server, tunnel},
    utils::tty::bold,
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

    let token = opts.secure.then(generate_token);
    let shared = build_shared(&manifest, &opts.path, token.clone())?;

    info!(
        "manifest size {}\n",
        format_bytes(shared.manifest_bytes.len() as u64)
    );

    let server = Server::bind(shared).await?;
    let port = server.port();
    let lan_url = format!("http://{}:{port}", lan_ip());

    let wan = tunnel::start(port).await;
    let wan_url = wan.as_ref().map(|t| t.url.as_str());
    let code = edge::remember_code(&opts.edge, wan_url, &opts.path).await;

    print_serve_details(
        &lan_url,
        wan_url,
        code.as_deref(),
        token.as_deref(),
        opts.local,
    );

    if let Err(e) = server.run_until_shutdown().await {
        error!("{e}");
    }
    if let Some(t) = wan {
        t.shutdown().await;
    }

    Ok(())
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
