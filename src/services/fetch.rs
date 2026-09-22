use std::{collections::HashMap, path::Path, sync::Arc, time::Duration};

use reqwest::Client;

use crate::{
    error,
    fs::system_time_from_ns,
    info,
    protocol::manifest::{CURRENT, Manifest, common::hex_encode, v1::MAGIC},
    utils::progress::{FetchProgress, format_bytes},
    warn,
};

pub const DEFAULT_TIMEOUT_MS: u32 = 60 * 1000; // 60 seconds
pub const MIN_TIMEOUT_MS: u32 = 15 * 1000; // 15 seconds
pub const MAX_TIMEOUT_MS: u32 = 120 * 1000; // 120 seconds
pub const RETRIES: u8 = 3;
pub const MAX_CONCURRENCY: u8 = 12;

pub struct FetchStats {
    pub files: usize,
    pub bytes: u64,
    pub blocks: usize,
    pub retries: u64,
    pub failed: usize,
}

pub async fn fetch(
    url: &str,
    dest: &Path,
    timeout: u32,
    bearer: Option<String>,
    force: bool,
    concurrency: u8,
    progress: &FetchProgress,
) -> Result<FetchStats, String> {
    let base = normalize_base(url)?;
    let mut builder = Client::builder().timeout(Duration::from_millis(timeout as u64));
    let host = base
        .strip_prefix("https://")
        .or_else(|| base.strip_prefix("http://"))
        .unwrap_or(&base)
        .split('/')
        .next()
        .unwrap_or("");
    if host.ends_with(".trycloudflare.com") {
        let state = crate::utils::dns::resolve_all_doh(host).await;
        if !state.addrs.is_empty() {
            builder = builder.resolve_to_addrs(host, &state.addrs);
        }
    }
    let client = builder.build().map_err(|e| format!("http client: {e}"))?;

    let (manifest, wire_len) = pull_manifest(&client, &base, bearer.as_deref()).await?;

    prepare_dest(dest, force)?;

    let mut out_files: Vec<Arc<std::fs::File>> = Vec::with_capacity(manifest.files.len());
    for f in &manifest.files {
        let abs = dest.join(&f.path);
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }

        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&abs)
            .map_err(|e| format!("create {}: {e}", abs.display()))?;
        crate::fs::preallocate(&file, f.size).map_err(|e| format!("prealloc {}: {e}", f.path))?;
        out_files.push(Arc::new(file));
    }

    let mut block_targets: HashMap<[u8; 32], Vec<(usize, u64)>> = HashMap::new();
    let bs = manifest.block_size as u64;
    for (fi, f) in manifest.files.iter().enumerate() {
        for (j, idx) in f.chunks.iter().enumerate() {
            let digest = manifest.pool[*idx as usize];
            let offset = j as u64 * bs;
            block_targets.entry(digest).or_default().push((fi, offset));
        }
    }

    let n_unique = block_targets.len();
    let bytes_total: u64 = manifest.files.iter().map(|f| f.size).sum();

    info!(
        "manifest file {} ({} B)\n{:7}{} files\n{:7}{}\n{:7}{} blocks\n{:7}protocol {} v{}",
        format_bytes(wire_len),
        wire_len,
        "",
        manifest.files.len(),
        "",
        format_bytes(bytes_total),
        "",
        n_unique,
        "",
        std::str::from_utf8(&MAGIC).unwrap_or("?"),
        CURRENT,
    );

    progress.begin(
        manifest
            .files
            .iter()
            .map(|f| (f.path.clone(), f.size))
            .collect(),
        n_unique as u64,
        bytes_total,
    );

    let out_files = Arc::new(out_files);
    let base = Arc::new(base);
    let sem = Arc::new(tokio::sync::Semaphore::new(concurrency.max(1) as usize));
    let mut tasks = Vec::with_capacity(n_unique);

    for (digest, locs) in block_targets {
        let files = out_files.clone();
        let token = bearer.clone();
        let client = client.clone();
        let base = base.clone();
        let sem = sem.clone();
        let prog = progress.clone();
        tasks.push(tokio::spawn(async move {
            let _permit = sem.acquire_owned().await.map_err(|e| e.to_string())?;
            let slot = prog.acquire_slot();
            let primary = locs.first().map(|(fi, _)| *fi);
            if let Some(fi) = primary {
                prog.slot_begin(slot, fi);
            }
            let hex = hex_encode(&digest);

            let mut last_err = "unreachable".to_string();
            for attempt in 1..=(1 + RETRIES) {
                match fetch_block(&client, &base, token.as_deref(), &hex).await {
                    Ok(data) => {
                        if blake3::hash(&data).as_bytes() != &digest {
                            last_err = format!("hash mismatch for block {hex}");
                        } else {
                            let mut primary_total = 0u64;
                            let mut primary_size = 0u64;
                            for (fi, off) in &locs {
                                let size = prog.file_size(*fi);
                                let add = (data.len() as u64).min(size.saturating_sub(*off));
                                let new_total = prog.add_file_bytes(*fi, add);
                                if let Err(e) =
                                    crate::fs::write_at(&files[*fi], &data[..add as usize], *off)
                                {
                                    prog.release_slot(slot);
                                    return Err(format!("write block {hex} at {off}: {e}"));
                                }
                                if Some(*fi) == primary {
                                    primary_total = new_total;
                                    primary_size = size;
                                }
                            }
                            prog.add_bytes(data.len() as u64);
                            prog.inc_blocks();
                            if let Some(fi) = primary {
                                if primary_total >= primary_size {
                                    prog.slot_done(slot, fi);
                                } else {
                                    prog.slot_progress(slot, fi);
                                }
                            }
                            prog.release_slot(slot);
                            return Ok::<(), String>(());
                        }
                    }
                    Err(e) => last_err = e,
                }
                if attempt <= RETRIES {
                    prog.inc_retries();
                    if let Some(fi) = primary {
                        prog.slot_retry(slot, fi, attempt + 1);
                    }
                    tokio::time::sleep(Duration::from_secs(attempt as u64)).await;
                }
            }
            prog.inc_failed();
            prog.slot_failed(slot, &hex, &last_err);
            prog.release_slot(slot);
            Err(format!("block {hex}: {last_err}"))
        }));
    }

    let mut failed = 0usize;
    let mut err_lines: Vec<String> = Vec::new();
    for t in tasks {
        match t.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                failed += 1;
                if err_lines.len() < 5 {
                    err_lines.push(e);
                }
            }
            Err(e) => {
                failed += 1;
                if err_lines.len() < 5 {
                    err_lines.push(format!("task panicked: {e}"));
                }
            }
        }
    }
    for e in err_lines {
        error!("{e}");
    }

    if failed > 0 {
        return Err(format!("{failed} block(s) failed — dest is incomplete"));
    }

    for (file, f) in out_files.iter().zip(manifest.files.iter()) {
        crate::fs::sync(file).map_err(|e| format!("fsync {}: {e}", f.path))?;
    }
    drop(out_files);

    for f in &manifest.files {
        let abs = dest.join(&f.path);
        let meta = crate::fs::metadata(&abs).map_err(|e| e.to_string())?;

        if let Some(mode) = f.mode
            && let Err(e) = crate::fs::apply_metadata(&abs, Some(mode), None)
        {
            warn!("chmod {}: {e}", abs.display());
        }

        if let Some(ns) = f.mtime_ns
            && let Some(_st) = system_time_from_ns(ns)
        {
            let cur_mtime = meta.mtime_ns;
            if cur_mtime != Some(ns)
                && let Err(e) = crate::fs::apply_metadata(&abs, None, Some(ns))
            {
                warn!("utime {}: {e}", abs.display());
            }
        }
    }

    Ok(FetchStats {
        files: manifest.files.len(),
        bytes: bytes_total,
        blocks: n_unique,
        retries: progress.retries(),
        failed: 0,
    })
}

pub fn normalize_base(url: &str) -> Result<String, String> {
    let s = url.trim();
    if s.starts_with("http://") || s.starts_with("https://") {
        return Ok(s.trim_end_matches('/').to_string());
    }
    if !s.contains("://") {
        return Err(format!("'{s}' is not a URL"));
    }
    Err("URL must be http:// or https://".into())
}

pub async fn pull_manifest(
    client: &Client,
    base: &str,
    token: Option<&str>,
) -> Result<(Manifest, u64), String> {
    let mut last_err = String::new();
    for attempt in 1..=(1 + RETRIES) {
        let mut req = client.get(format!("{base}/__rh__/manifest"));
        if let Some(t) = token {
            req = req.header("Authorization", format!("Bearer {t}"));
        }

        match req.send().await {
            Ok(res) => match res.status() {
                s if s.is_success() => {
                    let bytes = res
                        .bytes()
                        .await
                        .map_err(|e| format!("read manifest body: {e}"))?;
                    let wire_len = bytes.len() as u64;
                    let bytes = bytes.to_vec();
                    let manifest = crate::protocol::manifest::decode(&bytes)
                        .map_err(|e| format!("decode manifest: {e}"))?;

                    return Ok((manifest, wire_len));
                }
                s if s.as_u16() == 401 => {
                    return Err("manifest: 401 — wrong or missing --token".into());
                }
                s => last_err = format!("manifest: edge returned {s}"),
            },
            Err(e) => last_err = format!("manifest: {e}"),
        }
        if attempt <= RETRIES {
            error!(
                "manifest: not reachable (attempt {attempt}/{}), retrying...",
                RETRIES + 1
            );
            // manifest retries use 5×attempt delays (5s/10s/15s) — cold edge routes need longer warm-up
            tokio::time::sleep(Duration::from_secs(5 * attempt as u64)).await;
        }
    }

    Err(last_err)
}

fn prepare_dest(dest: &Path, force: bool) -> Result<(), String> {
    if dest.exists() {
        if !dest.is_dir() {
            return Err(format!(
                "dest {} exists and is not a directory",
                dest.display()
            ));
        }

        let non_empty = std::fs::read_dir(dest)
            .map_err(|e| e.to_string())?
            .next()
            .is_some();
        if non_empty && !force {
            return Err(format!(
                "dest {} is not empty — pass --force to fetch into it anyway",
                dest.display()
            ));
        }
    } else {
        std::fs::create_dir_all(dest).map_err(|e| format!("mkdir {}: {e}", dest.display()))?;
    }
    Ok(())
}

async fn fetch_block(
    client: &reqwest::Client,
    base: &str,
    token: Option<&str>,
    hex: &str,
) -> Result<Vec<u8>, String> {
    let mut req = client.get(format!("{base}/__rh__/block/{hex}"));
    if let Some(t) = token {
        req = req.header("Authorization", format!("Bearer {t}"));
    }

    let res = req
        .send()
        .await
        .map_err(|e| format!("GET block {hex}: {e}"))?;
    match res.status() {
        s if s.is_success() => Ok(res.bytes().await.map_err(|e| e.to_string())?.to_vec()),
        s if s.as_u16() == 401 => Err(format!("block {hex}: 401 — wrong --token")),
        s if s.as_u16() == 404 => Err(format!("block {hex}: unknown to sender")),
        s => Err(format!("block {hex}: edge returned {s}")),
    }
}
