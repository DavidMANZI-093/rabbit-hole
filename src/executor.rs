use std::{path::PathBuf, sync::Arc};

use crate::{
    cli::Command,
    error, info,
    report::{FetchSummary, print_cloudflared_status, print_fetch_summary, print_serve_details},
    services::{
        cloudflared, edge,
        fetch::fetch,
        ingest::ingest,
        serve::Server,
        share::{Shared, build_shared, generate_token},
        tunnel,
    },
    utils::{
        format::format_bytes,
        net::lan_ip,
        progress::{FetchProgress, IngestProgress},
    },
    warn,
};

impl Command {
    pub async fn execute(self) -> Result<(), String> {
        match self {
            Command::Serve {
                path,
                block_size,
                secure,
                edge,
                local,
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
            Command::Check => run_check(),
        }
    }
}

// ---------- commands ----------

struct ServeOpts {
    path: PathBuf,
    block_size: u32,
    secure: bool,
    local: bool,
    edge: String,
}

async fn run_serve(opts: ServeOpts) -> Result<(), String> {
    let progress = IngestProgress::new(&opts.path);
    let task_progress = progress.clone();
    let task_path = opts.path.clone();
    let block_size = opts.block_size;

    let (manifest, stats) = tokio::task::spawn_blocking(move || {
        ingest(&task_path, block_size, &task_progress).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())??;
    progress.finish_and_clear();

    if stats.skipped > 0 {
        warn!(
            "skipped {} entries, rerun with '--verbose' for reasons",
            stats.skipped
        );
    }
    info!("{}", stats.summary());

    let token: Option<String> = opts.secure.then(generate_token);
    let shared: Arc<Shared> = build_shared(&manifest, &opts.path, token.clone())?;

    info!(
        "manifest size {}\n",
        format_bytes(shared.manifest_bytes.len() as u64)
    );

    let server = Server::bind(shared).await?;
    let port = server.port();
    let lan_url: String = format!("http://{}:{port}", lan_ip());

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

struct FetchOpts {
    code_or_url: String,
    dest: PathBuf,
    timeout: u32,
    bearer: Option<String>,
    force: bool,
    concurrency: u8,
    edge: String,
}

async fn run_fetch(opts: FetchOpts) -> Result<(), String> {
    let url = resolve(&opts.code_or_url, &opts.edge).await?;
    info!("fetching from {}", url);

    let progress = FetchProgress::new();
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
    let snapshot = progress.snapshot();
    progress.finish_and_clear();

    match result {
        Ok(stats) => {
            print_fetch_summary(&FetchSummary::ok(&stats), &opts.dest);
            Ok(())
        }
        Err(e) => {
            if snapshot.started {
                print_fetch_summary(&FetchSummary::incomplete(&snapshot), &opts.dest);
            }
            Err(e)
        }
    }
}

fn run_check() -> Result<(), String> {
    print_cloudflared_status(&cloudflared::status());
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
