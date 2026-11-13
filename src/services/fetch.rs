// Copyright (c) 2026 David MANZI
// SPDX-License-Identifier: MIT

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use reqwest::Client;

use crate::{
    debug, info,
    protocol::{
        self,
        manifest::{
            CURRENT, Manifest,
            common::{ManifestError, hex_encode},
            v1::MAGIC,
        },
    },
    utils::{
        self, dns::DynamicDnsState, format::format_bytes, fs::system_time_from_ns,
        progress::FetchProgress,
    },
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
    let base: String = normalize_base(url)?;
    let client: reqwest::Client = build_client(&base, timeout).await?;
    let (manifest, raw_len) = pull_manifest(&client, &base, bearer.as_deref()).await?;
    let manifest = Arc::new(manifest);

    let (files, targets) = {
        let dest = dest.to_path_buf();
        let manifest = manifest.clone();
        tokio::task::spawn_blocking(move || {
            let preflight: Preflight = preflight_dest(&dest, &manifest, force)?;
            let files: Vec<OutputFile> = create_outputs(&dest, &manifest, &preflight.needs_write)?;
            let targets: HashMap<[u8; 32], Vec<(usize, u64)>> =
                plan_blocks(&manifest, &preflight.verified);
            Ok::<_, String>((files, targets))
        })
        .await
        .map_err(|e| format!("prepare: task panicked: {e}"))
        .and_then(|r| r)?
    };
    let files: Arc<Vec<OutputFile>> = Arc::new(files);

    let n_unique: usize = targets.len();
    let bytes_total: u64 = manifest.files.iter().map(|f| f.size).sum();
    info!(
        "manifest size {}, {}:v{} protocol\nfiles {} ({} total, {} blocks to fetch)",
        format_bytes(raw_len),
        std::str::from_utf8(&MAGIC).unwrap_or("?"),
        CURRENT,
        manifest.files.len(),
        format_bytes(bytes_total),
        n_unique,
    );

    progress.begin(manifest.files.len(), n_unique as u64, bytes_total);
    let fetcher = Fetcher::new(
        client,
        &base,
        bearer,
        files.clone(),
        concurrency,
        progress.clone(),
    );
    fetcher.run(targets).await?;

    {
        let dest = dest.to_path_buf();
        let manifest = manifest.clone();
        tokio::task::spawn_blocking(move || {
            sync_outputs(files.as_slice())?;
            drop(files);
            restore_metadata(&dest, &manifest)
        })
        .await
        .map_err(|e| format!("finalize: task panicked: {e}"))
        .and_then(|r| r.map_err(|e| format!("finalize: {e}")))?;
    }

    Ok(FetchStats {
        files: manifest.files.len(),
        bytes: bytes_total,
        blocks: n_unique,
        retries: progress.snapshot().retries,
    })
}

async fn build_client(base: &str, timeout: u32) -> Result<Client, String> {
    let mut builder = Client::builder().timeout(Duration::from_millis(timeout as u64));
    if let Some(host) = tunnel_host(base) {
        let state: DynamicDnsState = utils::dns::resolve_all_doh(host).await;
        if !state.addrs.is_empty() {
            debug!(
                "dns: resolved {host} to {} addr(s) via DoH",
                state.addrs.len()
            );
            builder = builder.resolve_to_addrs(host, &state.addrs);
        } else {
            debug!("dns: no DoH results for {host}, using system resolver");
        }
    }
    builder
        .build()
        .map_err(|e| format!("http client (timeout {}ms): {e}", timeout))
}

fn tunnel_host(base: &str) -> Option<&str> {
    let rest: &str = base
        .strip_prefix("https://")
        .or_else(|| base.strip_prefix("http://"))?;
    let host: &str = rest.split('/').next().unwrap_or("");
    host.ends_with(".trycloudflare.com").then_some(host)
}

struct OutputFile {
    file: Option<std::fs::File>,
    path: String,
    size: u64,
}

fn create_outputs(
    dest: &Path,
    manifest: &Manifest,
    needs_write: &[bool],
) -> Result<Vec<OutputFile>, String> {
    let mut out: Vec<OutputFile> = Vec::with_capacity(manifest.files.len());
    for (fi, f) in manifest.files.iter().enumerate() {
        if !needs_write[fi] {
            out.push(OutputFile {
                file: None,
                path: f.path.clone(),
                size: f.size,
            });
            continue;
        }

        let abs: PathBuf = dest.join(&f.path);
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }

        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&abs)
            .map_err(|e| format!("create {}: {e}", abs.display()))?;

        utils::fs::preallocate(&file, f.size).map_err(|e| format!("prealloc {}: {e}", f.path))?;
        out.push(OutputFile {
            file: Some(file),
            path: f.path.clone(),
            size: f.size,
        });
    }
    Ok(out)
}

type BlockTargets = HashMap<[u8; 32], Vec<(usize, u64)>>;

fn plan_blocks(manifest: &Manifest, verified: &[Vec<bool>]) -> BlockTargets {
    let bs = manifest.block_size as u64;
    let mut targets = BlockTargets::new();
    for (fi, f) in manifest.files.iter().enumerate() {
        for (j, idx) in f.chunks.iter().enumerate() {
            if verified[fi][j] {
                continue;
            }
            let digest = manifest.pool[*idx as usize];
            targets.entry(digest).or_default().push((fi, j as u64 * bs));
        }
    }
    targets
}

#[derive(Clone)]
struct Fetcher {
    client: Client,
    base: Arc<str>,
    token: Option<Arc<str>>,
    files: Arc<Vec<OutputFile>>,
    concurrency: usize,
    progress: FetchProgress,
}

impl Fetcher {
    fn new(
        client: Client,
        base: &str,
        token: Option<String>,
        files: Arc<Vec<OutputFile>>,
        concurrency: u8,
        progress: FetchProgress,
    ) -> Self {
        Self {
            client,
            base: Arc::from(base),
            token: token.map(Arc::from),
            files,
            concurrency: concurrency.max(1) as usize,
            progress,
        }
    }

    async fn run(self, targets: BlockTargets) -> Result<(), String> {
        let total = targets.len();
        let mut set = tokio::task::JoinSet::new();
        let mut failed = 0usize;
        let mut err_lines: Vec<String> = Vec::new();

        for (digest, locs) in targets {
            if set.len() >= self.concurrency
                && let Some(res) = set.join_next().await
            {
                record_task(res, &self.progress, &mut failed, &mut err_lines);
            }

            let fetcher: Fetcher = self.clone();
            set.spawn(async move { fetcher.download(digest, &locs).await });
        }

        while let Some(res) = set.join_next().await {
            record_task(res, &self.progress, &mut failed, &mut err_lines);
        }

        if failed == 0 {
            Ok(())
        } else {
            let hidden = failed.saturating_sub(err_lines.len());
            let mut msg = format!("{failed}/{total} block(s) failed — dest is incomplete");
            for e in &err_lines {
                msg.push_str(&format!("\n  {e}"));
            }
            if hidden > 0 {
                msg.push_str(&format!(
                    "\n  (and {hidden} more — rerun with --verbose for details)"
                ));
            }
            Err(msg)
        }
    }

    async fn download(&self, digest: [u8; 32], locs: &[(usize, u64)]) -> Result<(), String> {
        let result: Result<(), String> = self.try_download(digest, locs).await;
        if result.is_err() {
            self.progress.inc_failed();
        }
        result
    }

    async fn try_download(&self, digest: [u8; 32], locs: &[(usize, u64)]) -> Result<(), String> {
        let hex: String = hex_encode(&digest);
        let short = &hex[..12]; // first 12 hex chars (48 bits) for user-facing messages
        let mut last_err = String::new();

        for attempt in 1..=(1 + RETRIES) {
            debug!("block {hex}: attempt {attempt}/{}", 1 + RETRIES);
            match fetch_block(
                &self.client,
                &self.base,
                self.token.as_deref(),
                &hex,
                &digest,
            )
            .await
            {
                Ok(data) => {
                    let data_len = data.len() as u64;
                    let files: Arc<Vec<OutputFile>> = Arc::clone(&self.files);
                    let locs: Vec<(usize, u64)> = locs.to_vec();
                    let write_hex: String = hex.clone();
                    tokio::task::spawn_blocking(move || {
                        write_block(&files, &data, &locs, &write_hex)
                    })
                    .await
                    .map_err(|e| format!("block {short}…: write task: {e}"))??;
                    self.progress.add_bytes(data_len);
                    self.progress.inc_blocks();
                    return Ok(());
                }
                Err(BlockError::Fatal(e)) => {
                    last_err = e;
                    break;
                }
                Err(BlockError::Retryable(e)) => {
                    debug!("block {hex}: retryable: {e}");
                    last_err = e;
                }
            }
            if attempt <= RETRIES {
                self.progress.inc_retries();
                let delay = attempt as u64;
                debug!("block {hex}: retry in {delay}s");
                tokio::time::sleep(Duration::from_secs(delay)).await;
            }
        }

        Err(last_err)
    }
}

const MAX_REPORTED_ERRORS: usize = 5;

fn write_block(
    files: &[OutputFile],
    data: &[u8],
    locs: &[(usize, u64)],
    hex: &str,
) -> Result<(), String> {
    let short = &hex[..12.min(hex.len())];
    for &(fi, off) in locs {
        let target: &OutputFile = &files[fi];
        let Some(file) = target.file.as_ref() else {
            return Err(format!(
                "internal: no writable output for {} at offset {off}",
                target.path
            ));
        };
        let len = (data.len() as u64).min(target.size.saturating_sub(off));
        if let Err(e) = utils::fs::write_at(file, &data[..len as usize], off) {
            return Err(format!(
                "block {short}…: write at {off} in {}: {e}",
                target.path
            ));
        }
    }
    Ok(())
}

fn record_task(
    res: Result<Result<(), String>, tokio::task::JoinError>,
    progress: &FetchProgress,
    failed: &mut usize,
    err_lines: &mut Vec<String>,
) {
    match res {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            *failed += 1;
            remember_error(err_lines, e);
        }
        Err(e) => {
            *failed += 1;
            progress.inc_failed();
            remember_error(err_lines, format!("task panicked: {e}"));
        }
    }
}

fn remember_error(lines: &mut Vec<String>, e: String) {
    if lines.len() < MAX_REPORTED_ERRORS {
        lines.push(e);
    }
}

fn sync_outputs(files: &[OutputFile]) -> Result<(), String> {
    for f in files {
        if let Some(file) = f.file.as_ref() {
            utils::fs::sync(file).map_err(|e| format!("fsync {}: {e}", f.path))?;
        }
    }
    Ok(())
}

fn restore_metadata(dest: &Path, manifest: &Manifest) -> Result<(), String> {
    for f in &manifest.files {
        let abs = dest.join(&f.path);
        let meta = utils::fs::metadata(&abs).map_err(|e| e.to_string())?;

        if let Some(mode) = f.mode
            && let Err(e) = utils::fs::apply_metadata(&abs, Some(mode), None)
        {
            warn!("chmod {}: {e}", abs.display());
        }

        if let Some(ns) = f.mtime_ns
            && let Some(_st) = system_time_from_ns(ns)
            && meta.mtime_ns != Some(ns)
            && let Err(e) = utils::fs::apply_metadata(&abs, None, Some(ns))
        {
            warn!("utime {}: {e}", abs.display());
        }
    }
    Ok(())
}

pub fn normalize_base(url: &str) -> Result<String, String> {
    let s: &str = url.trim();
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
        debug!("manifest: attempt {attempt}/{} from {base}", 1 + RETRIES);
        let mut req: reqwest::RequestBuilder = client.get(format!("{base}/__rh__/manifest"));
        if let Some(t) = token {
            req = req.header("Authorization", format!("Bearer {t}"));
        }

        match req.send().await {
            Ok(res) => match res.status() {
                // 200 OK
                s if s.is_success() => {
                    let bytes = res
                        .bytes()
                        .await
                        .map_err(|e| format!("read manifest body: {e}"))?;
                    let raw_len = bytes.len() as u64;
                    let bytes = bytes.to_vec();
                    let manifest = protocol::manifest::decode(&bytes).map_err(decode_error)?;

                    return Ok((manifest, raw_len));
                }
                // 401 UNAUTHORIZED
                s if s.as_u16() == 401 => {
                    return Err(
                        "manifest: 401 — wrong or missing bearer token (pass --bearer <token>)"
                            .into(),
                    );
                }
                // OTHERS
                s => {
                    debug!("manifest: HTTP {s}");
                    last_err = format!("manifest: HTTP {s}");
                }
            },
            Err(e) => {
                debug!("manifest: send failed: {e}");
                last_err = format!("manifest: {e}");
            }
        }
        if attempt <= RETRIES {
            warn!(
                "manifest: not reachable (attempt {attempt}/{}), retrying...",
                RETRIES + 1
            );
            // manifest retries use 'attempt * 5' delays (5s/10s/15s)
            tokio::time::sleep(Duration::from_secs(5 * attempt as u64)).await;
        }
    }

    Err(last_err)
}

fn decode_error(e: ManifestError) -> String {
    match e {
        ManifestError::UnsupportedVersion(v) => {
            format!("share uses manifest v{v} (needs newer rh — upgrade and retry)")
        }
        e => format!("decode manifest: {e}"),
    }
}

#[derive(Debug)]
struct Preflight {
    verified: Vec<Vec<bool>>,
    needs_write: Vec<bool>,
    conflicts: Vec<PathBuf>,
}

fn preflight_dest(dest: &Path, manifest: &Manifest, force: bool) -> Result<Preflight, String> {
    if dest.exists() {
        if !dest.is_dir() {
            return Err(format!(
                "dest {} exists and is not a directory",
                dest.display()
            ));
        }
    } else {
        std::fs::create_dir_all(dest).map_err(|e| format!("mkdir {}: {e}", dest.display()))?;
    }

    let block_size = manifest.block_size as u64;
    let mut preflight = Preflight {
        verified: manifest
            .files
            .iter()
            .map(|f| vec![false; f.chunks.len()])
            .collect(),
        needs_write: vec![false; manifest.files.len()],
        conflicts: Vec::new(),
    };

    for (idx, f) in manifest.files.iter().enumerate() {
        let abs = dest.join(&f.path);
        let meta = match std::fs::symlink_metadata(&abs) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                preflight.needs_write[idx] = true;
                continue;
            }
            Err(e) => return Err(format!("stat {}: {e}", abs.display())),
        };
        if !meta.file_type().is_file() {
            return Err(format!(
                "target {} exists and is not a regular file; remove it manually",
                abs.display()
            ));
        }

        let file = std::fs::File::open(&abs).map_err(|e| format!("open {}: {e}", abs.display()))?;
        let file_len = meta.len();
        let mut all_match = file_len == f.size;
        for (nth, chunk) in f.chunks.iter().enumerate() {
            let offset = nth as u64 * block_size;
            let len = block_size.min(f.size.saturating_sub(offset)) as usize;
            if file_len >= offset + len as u64
                && block_matches(&file, offset, len, &manifest.pool[*chunk as usize])
            {
                // 'idx': file index, 'nth': chunk index.
                preflight.verified[idx][nth] = true;
            } else {
                all_match = false;
            }
        }
        if !all_match {
            preflight.needs_write[idx] = true;
            preflight.conflicts.push(abs);
        }
    }

    if !preflight.conflicts.is_empty() && !force {
        let mut msg = format!(
            "dest {} contains {} target file(s) that differ from the share:",
            dest.display(),
            preflight.conflicts.len()
        );
        for p in preflight.conflicts.iter().take(5) {
            msg.push_str(&format!("\n  {}", p.display()));
        }
        if preflight.conflicts.len() > 5 {
            msg.push_str(&format!(
                "\n  ... and {} more",
                preflight.conflicts.len() - 5
            ));
        }
        msg.push_str("\npass --force to replace only those files");
        return Err(msg);
    }

    let needs = preflight.needs_write.iter().filter(|&&b| b).count();
    let skipped = manifest.files.len() - needs;
    let verified_blocks: usize = preflight
        .verified
        .iter()
        .flat_map(|v| v.iter())
        .filter(|&&b| b)
        .count();
    debug!(
        "preflight: {needs} file(s) to write, {skipped} already match, {verified_blocks} block(s) verified"
    );

    Ok(preflight)
}

fn block_matches(file: &std::fs::File, offset: u64, len: usize, want: &[u8; 32]) -> bool {
    let mut buf = vec![0u8; len];
    let mut done = 0;
    while done < len {
        match utils::fs::read_at(file, &mut buf[done..], offset + done as u64) {
            Ok(0) => return false,
            Ok(n) => done += n,
            Err(_) => return false,
        }
    }
    blake3::hash(&buf).as_bytes() == want
}

enum BlockError {
    Retryable(String),
    Fatal(String),
}

async fn fetch_block(
    client: &reqwest::Client,
    base: &str,
    token: Option<&str>,
    hex: &str,
    digest: &[u8; 32],
) -> Result<Vec<u8>, BlockError> {
    let short = &hex[..12.min(hex.len())];
    let mut req = client.get(format!("{base}/__rh__/block/{hex}"));
    if let Some(t) = token {
        req = req.header("Authorization", format!("Bearer {t}"));
    }

    let res = req
        .send()
        .await
        .map_err(|e| BlockError::Retryable(format!("block {short}…: {e}")))?;
    let status = res.status();
    debug!("block {hex}: HTTP {status}");
    match status {
        s if s.is_success() => {
            let data =
                res.bytes().await.map(|b| b.to_vec()).map_err(|e| {
                    BlockError::Retryable(format!("block {short}…: read body: {e}"))
                })?;
            if blake3::hash(&data).as_bytes() != digest {
                return Err(BlockError::Retryable(format!(
                    "block {short}…: hash mismatch"
                )));
            }
            Ok(data)
        }
        s if s.as_u16() == 401 => Err(BlockError::Fatal(format!(
            "block {short}…: 401 — wrong or missing bearer token (pass --bearer <token>)"
        ))),
        s if s.as_u16() == 404 => Err(BlockError::Fatal(format!(
            "block {short}…: not found on sender (sender may have stopped)"
        ))),
        s => Err(BlockError::Retryable(format!("block {short}…: HTTP {s}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::manifest::FileEntry;

    #[test]
    fn upgrade_message_names_version() {
        let msg = decode_error(ManifestError::UnsupportedVersion(2));
        assert!(msg.contains('2'), "unexpected message: {msg}");
        assert!(msg.contains("newer rh"), "unexpected message: {msg}");
    }

    #[test]
    fn other_decode_errors_report_parse_failure() {
        let msg = decode_error(ManifestError::BadMagic);
        assert!(
            msg.starts_with("decode manifest:"),
            "unexpected message: {msg}"
        );
    }

    #[tokio::test]
    async fn garbage_manifest_fails_before_touching_dest() {
        // Decode failure returns via `?` immediately (no retry backoff).
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let port = listener.local_addr().expect("local addr").port();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                use tokio::io::AsyncWriteExt;
                let _ = sock
                    .write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-length: 3\r\nconnection: close\r\n\r\nXYZ",
                    )
                    .await;
            }
        });
        let dest =
            std::env::temp_dir().join(format!("rh-failclosed-{}-{port}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dest);

        let progress = FetchProgress::new();
        let err = match fetch(
            &format!("http://127.0.0.1:{port}"),
            &dest,
            MIN_TIMEOUT_MS,
            None,
            false,
            1,
            &progress,
        )
        .await
        {
            Ok(_) => panic!("expected decode failure"),
            Err(e) => e,
        };
        assert!(err.contains("decode manifest"), "unexpected error: {err}");
        assert!(!dest.exists(), "dest must stay untouched on decode failure");
        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn strips_trailing_slash_from_https_url() {
        assert_eq!(
            normalize_base("https://example.com/").unwrap(),
            "https://example.com"
        );
    }

    #[test]
    fn strips_multiple_trailing_slashes() {
        assert_eq!(
            normalize_base("https://example.com///").unwrap(),
            "https://example.com"
        );
    }

    #[test]
    fn accepts_http_scheme() {
        assert_eq!(
            normalize_base("http://localhost:8080").unwrap(),
            "http://localhost:8080"
        );
    }

    #[test]
    fn trims_surrounding_whitespace() {
        assert_eq!(
            normalize_base("  https://example.com  ").unwrap(),
            "https://example.com"
        );
    }

    #[test]
    fn rejects_missing_scheme() {
        assert!(normalize_base("example.com").is_err());
    }

    #[test]
    fn rejects_ftp_scheme() {
        assert!(normalize_base("ftp://example.com").is_err());
    }

    #[test]
    fn rejects_empty_string() {
        assert!(normalize_base("").is_err());
    }

    #[test]
    fn tunnel_host_matches_only_trycloudflare() {
        assert_eq!(
            tunnel_host("https://abc.trycloudflare.com"),
            Some("abc.trycloudflare.com")
        );
        assert_eq!(tunnel_host("http://127.0.0.1:8080"), None);
        assert_eq!(tunnel_host("https://example.com/x"), None);
    }

    #[test]
    fn plan_blocks_deduplicates_repeated_digests() {
        let first = [1u8; 32];
        let second = [2u8; 32];
        let manifest = Manifest {
            block_size: 4096,
            pool: vec![first, second],
            root: None,
            files: vec![
                FileEntry {
                    path: "a".into(),
                    size: 8192,
                    mode: None,
                    mtime_ns: None,
                    chunks: vec![0, 1],
                },
                FileEntry {
                    path: "b".into(),
                    size: 4096,
                    mode: None,
                    mtime_ns: None,
                    chunks: vec![0],
                },
            ],
        };

        let targets = plan_blocks(&manifest, &[vec![false, false], vec![false]]);
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[&first], vec![(0, 0), (1, 0)]);
        assert_eq!(targets[&second], vec![(0, 4096)]);
    }

    #[test]
    fn plan_blocks_skips_verified_locations() {
        let first = [1u8; 32];
        let second = [2u8; 32];
        let manifest = Manifest {
            block_size: 4096,
            pool: vec![first, second],
            root: None,
            files: vec![
                FileEntry {
                    path: "a".into(),
                    size: 8192,
                    mode: None,
                    mtime_ns: None,
                    chunks: vec![0, 1],
                },
                FileEntry {
                    path: "b".into(),
                    size: 4096,
                    mode: None,
                    mtime_ns: None,
                    chunks: vec![0],
                },
            ],
        };

        let targets = plan_blocks(&manifest, &[vec![false, true], vec![true]]);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[&first], vec![(0, 0)]);
        assert!(!targets.contains_key(&second));
    }

    #[test]
    fn plan_blocks_keeps_shared_digest_when_one_location_is_missing() {
        let first = [1u8; 32];
        let second = [2u8; 32];
        let manifest = Manifest {
            block_size: 4096,
            pool: vec![first, second],
            root: None,
            files: vec![
                FileEntry {
                    path: "a".into(),
                    size: 8192,
                    mode: None,
                    mtime_ns: None,
                    chunks: vec![0, 1],
                },
                FileEntry {
                    path: "b".into(),
                    size: 4096,
                    mode: None,
                    mtime_ns: None,
                    chunks: vec![0],
                },
            ],
        };

        let targets = plan_blocks(&manifest, &[vec![true, false], vec![false]]);
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[&first], vec![(1, 0)]);
        assert_eq!(targets[&second], vec![(0, 4096)]);
    }

    #[test]
    fn create_outputs_makes_nested_dirs_and_preallocates() {
        let dir = std::env::temp_dir().join(format!("rh-create-outputs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let manifest = Manifest {
            block_size: 4096,
            pool: vec![],
            root: None,
            files: vec![FileEntry {
                path: "nested/a.txt".into(),
                size: 12,
                mode: None,
                mtime_ns: None,
                chunks: vec![],
            }],
        };

        let files = create_outputs(&dir, &manifest, &[true]).expect("create outputs");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "nested/a.txt");
        assert_eq!(files[0].size, 12);
        assert_eq!(
            std::fs::metadata(dir.join("nested/a.txt"))
                .expect("metadata")
                .len(),
            12
        );

        drop(files);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn create_outputs_skips_files_that_need_no_write() {
        let dir = temp_dir("skip-output");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), b"hello").unwrap();
        let manifest = one_block_manifest(b"hello");

        let files = create_outputs(&dir, &manifest, &[false]).expect("create outputs");
        assert_eq!(files.len(), 1);
        assert!(files[0].file.is_none());
        assert_eq!(std::fs::read(dir.join("a.txt")).unwrap(), b"hello");

        drop(files);
        std::fs::remove_dir_all(&dir).ok();
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rh-preflight-{}-{tag}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn one_block_manifest(bytes: &[u8]) -> Manifest {
        Manifest {
            block_size: 4096,
            pool: vec![*blake3::hash(bytes).as_bytes()],
            root: None,
            files: vec![FileEntry {
                path: "a.txt".into(),
                size: bytes.len() as u64,
                mode: None,
                mtime_ns: None,
                chunks: vec![0],
            }],
        }
    }

    #[cfg(unix)]
    #[test]
    fn restore_metadata_strips_special_mode_bits() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temp_dir("special-bits");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), b"hello").unwrap();
        let mut manifest = one_block_manifest(b"hello");
        manifest.files[0].mode = Some(0o6755);

        restore_metadata(&dir, &manifest).expect("restore metadata");
        let mode = std::fs::metadata(dir.join("a.txt"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o7000, 0, "setuid/setgid/sticky must not survive");
        assert_eq!(mode & 0o777, 0o755);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn preflight_creates_missing_directory() {
        let dir = temp_dir("missing");
        let manifest = one_block_manifest(b"hello");
        let preflight = preflight_dest(&dir, &manifest, false).expect("preflight");
        assert!(dir.is_dir());
        assert!(preflight.conflicts.is_empty());
        assert!(!preflight.verified[0][0]);
        assert!(preflight.needs_write[0]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn preflight_accepts_existing_empty_directory() {
        let dir = temp_dir("empty");
        std::fs::create_dir_all(&dir).unwrap();
        let manifest = one_block_manifest(b"hello");
        let preflight = preflight_dest(&dir, &manifest, false).expect("preflight");
        assert!(preflight.conflicts.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn preflight_ignores_unrelated_files() {
        let dir = temp_dir("unrelated");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("unrelated.txt"), b"keep me").unwrap();
        let manifest = one_block_manifest(b"hello");
        let preflight = preflight_dest(&dir, &manifest, false).expect("preflight");
        assert!(preflight.conflicts.is_empty());
        assert_eq!(
            std::fs::read(dir.join("unrelated.txt")).unwrap(),
            b"keep me"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn preflight_accepts_matching_target_without_force() {
        let dir = temp_dir("matching");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), b"hello").unwrap();
        let manifest = one_block_manifest(b"hello");
        let preflight = preflight_dest(&dir, &manifest, false).expect("preflight");
        assert!(preflight.conflicts.is_empty());
        assert!(preflight.verified[0][0]);
        assert!(!preflight.needs_write[0]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn preflight_rejects_differing_target_without_force() {
        let dir = temp_dir("differing");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), b"other").unwrap();
        let manifest = one_block_manifest(b"hello");
        let err = preflight_dest(&dir, &manifest, false).expect_err("must conflict");
        assert!(err.contains("a.txt"), "unexpected error: {err}");
        assert!(err.contains("--force"), "unexpected error: {err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn preflight_accepts_differing_target_with_force() {
        let dir = temp_dir("force");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), b"other").unwrap();
        let manifest = one_block_manifest(b"hello");
        let preflight = preflight_dest(&dir, &manifest, true).expect("preflight");
        assert_eq!(preflight.conflicts.len(), 1);
        assert!(!preflight.verified[0][0]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn preflight_rejects_non_regular_target() {
        let dir = temp_dir("nonregular");
        std::fs::create_dir_all(dir.join("a.txt")).unwrap();
        let manifest = one_block_manifest(b"hello");
        assert!(preflight_dest(&dir, &manifest, true).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn preflight_rejects_dest_that_is_a_file() {
        let dir = temp_dir("destfile");
        std::fs::write(&dir, b"data").unwrap();
        let manifest = one_block_manifest(b"hello");
        assert!(preflight_dest(&dir, &manifest, false).is_err());
        std::fs::remove_file(&dir).ok();
    }
}
