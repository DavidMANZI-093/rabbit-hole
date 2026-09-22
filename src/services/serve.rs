use std::{
    collections::{BTreeMap, HashMap},
    io::SeekFrom,
    path::{Path, PathBuf},
    sync::Arc,
};

use axum::{
    Router,
    body::Body,
    extract::{Path as UrlPath, Query, State},
    http::{HeaderMap, StatusCode, Uri, header},
    response::{Html, IntoResponse, Response},
    routing::get,
};
use rand::Rng;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;

use crate::protocol::{self, manifest::Manifest};

#[derive(Clone)]
pub struct Locator {
    pub abs: PathBuf,
    pub offset: u64,
    pub len: u32,
}

pub struct DirFile {
    pub rel: String,
    pub size: u64,
}

pub enum Kind {
    Single {
        abs: PathBuf,
        name: String,
        size: u64,
    },
    Dir {
        root: PathBuf,
        files: Vec<DirFile>,
    },
}

pub struct Shared {
    pub manifest_bytes: Vec<u8>,
    pub etag: String,
    pub kind: Kind,
    pub blocks: HashMap<[u8; 32], Arc<Vec<Locator>>>,
    pub token: Option<String>,
}

pub fn build_shared(
    manifest: &Manifest,
    src: &Path,
    token: Option<String>,
) -> Result<Arc<Shared>, String> {
    let manifest_bytes = protocol::manifest::encode(manifest);
    let etag = blake3::hash(&manifest_bytes).to_hex().to_string();
    let bs = manifest.block_size as u64;
    let single = src.is_file();

    let mut blocks_build: HashMap<[u8; 32], Vec<Locator>> = HashMap::new();
    for f in &manifest.files {
        let abs = if single {
            src.to_path_buf()
        } else {
            src.join(&f.path)
        };

        for (j, idx) in f.chunks.iter().enumerate() {
            let digest = manifest.pool[*idx as usize];
            let offset = j as u64 * bs;
            let len = bs.min(f.size.saturating_sub(offset)) as u32;

            blocks_build.entry(digest).or_default().push(Locator {
                abs: abs.clone(),
                offset,
                len,
            });
        }
    }
    let blocks: HashMap<[u8; 32], Arc<Vec<Locator>>> = blocks_build
        .into_iter()
        .map(|(k, v)| (k, Arc::new(v)))
        .collect();

    let kind = if single {
        let entry = manifest
            .files
            .first()
            .ok_or("single-file manifest must contain exactly one file")?;
        if manifest.files.len() != 1 {
            return Err("single-file share expects exactly one manifest entry".into());
        }

        let live = crate::fs::metadata(src).map_err(|e| format!("stat {}: {e}", src.display()))?;
        if live.kind != crate::fs::Kind::File {
            return Err(format!("refusing non-file {}", src.display()));
        }
        if live.size != entry.size {
            return Err(format!(
                "source changed since ingest (manifest {} B, disk {} B): re-share",
                entry.size, live.size
            ));
        }

        let name = src
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("download")
            .to_string();
        Kind::Single {
            abs: src.to_path_buf(),
            name,
            size: entry.size,
        }
    } else {
        Kind::Dir {
            root: src.to_path_buf(),
            files: manifest
                .files
                .iter()
                .map(|f| DirFile {
                    rel: f.path.clone(),
                    size: f.size,
                })
                .collect(),
        }
    };

    Ok(Arc::new(Shared {
        manifest_bytes,
        etag,
        kind,
        blocks,
        token,
    }))
}

pub fn router(shared: Arc<Shared>) -> Router {
    Router::new()
        .route("/", get(h_root))
        .route("/__rh__/file", get(h_file_single))
        .route("/__rh__/f/{*rel}", get(h_file_rel))
        .route("/__rh__/tree", get(h_tree_root))
        .route("/__rh__/tree/{*prefix}", get(h_tree_prefix))
        .route("/__rh__/manifest", get(h_manifest))
        .route("/__rh__/block/{:hash}", get(h_block))
        .route("/__rh__/health", get(|| async { "ok" }))
        .with_state(shared)
}

// ---------- handlers ----------

async fn h_root(
    State(s): State<Arc<Shared>>,
    Query(q): Query<HashMap<String, String>>,
    uri: Uri,
) -> Response {
    if !query_ok(&q, s.token.as_deref()) {
        return unauthorized();
    }
    match &s.kind {
        Kind::Single { .. } => {
            let qs = uri.query().map(|x| format!("?{x}")).unwrap_or_default();
            (
                StatusCode::FOUND,
                [(header::LOCATION, format!("/__rh__/file{qs}"))],
                Body::empty(),
            )
                .into_response()
        }
        Kind::Dir { .. } => render_tree(&s, "", &q).into_response(),
    }
}

async fn h_file_single(
    State(s): State<Arc<Shared>>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    if !query_ok(&q, s.token.as_deref()) {
        return unauthorized();
    }
    match &s.kind {
        Kind::Single { abs, name, size } => {
            let rh = headers
                .get(header::RANGE)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            stream_file_response(abs.clone(), name, *size, rh, &s.etag).await
        }
        Kind::Dir { .. } => {
            (StatusCode::NOT_FOUND, "directory share: use / to browse\n").into_response()
        }
    }
}

async fn h_file_rel(
    State(s): State<Arc<Shared>>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
    UrlPath(rel): UrlPath<String>,
) -> Response {
    if !query_ok(&q, s.token.as_deref()) {
        return unauthorized();
    }
    let Kind::Dir { root, files } = &s.kind else {
        return (StatusCode::NOT_FOUND, "single-file share\n").into_response();
    };
    let Some(meta) = files.iter().find(|f| f.rel == rel) else {
        return (StatusCode::NOT_FOUND, "no such file\n").into_response();
    };
    let name = rel.rsplit('/').next().unwrap_or(&rel).to_string();
    let rh = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    stream_file_response(root.join(&rel), &name, meta.size, rh, &s.etag).await
}

async fn h_tree_root(
    State(s): State<Arc<Shared>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    if !query_ok(&q, s.token.as_deref()) {
        return unauthorized();
    }
    if matches!(s.kind, Kind::Single { .. }) {
        return (
            StatusCode::FOUND,
            [(
                header::LOCATION,
                format!("/__rh__/file{}", pass_query(s.token.as_deref(), &q)),
            )],
            Body::empty(),
        )
            .into_response();
    }
    render_tree(&s, "", &q).into_response()
}

async fn h_tree_prefix(
    State(s): State<Arc<Shared>>,
    Query(q): Query<HashMap<String, String>>,
    UrlPath(prefix): UrlPath<String>,
) -> Response {
    if !query_ok(&q, s.token.as_deref()) {
        return unauthorized();
    }
    if matches!(s.kind, Kind::Single { .. }) {
        return (
            StatusCode::FOUND,
            [(
                header::LOCATION,
                format!("/__rh__/file{}", pass_query(s.token.as_deref(), &q)),
            )],
            Body::empty(),
        )
            .into_response();
    }
    render_tree(&s, prefix.trim_matches('/'), &q).into_response()
}

async fn h_manifest(
    State(s): State<Arc<Shared>>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    if !bearer_ok(&headers, &q, s.token.as_deref()) {
        return unauthorized();
    }
    Response::builder()
        .header(header::CONTENT_TYPE, "application/x-rhbm-v1")
        .header(header::CONTENT_LENGTH, s.manifest_bytes.len().to_string())
        .header(header::ETAG, format!("\"{}\"", s.etag))
        .body(Body::from(s.manifest_bytes.clone()))
        .unwrap_or_else(|_| (StatusCode::INTERNAL_SERVER_ERROR, "build failed\n").into_response())
}

async fn h_block(
    State(s): State<Arc<Shared>>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
    UrlPath(hash_hex): UrlPath<String>,
) -> Response {
    if !bearer_ok(&headers, &q, s.token.as_deref()) {
        return unauthorized();
    }
    let want = match crate::protocol::manifest::common::hex_decode32(&hash_hex.to_lowercase()) {
        Ok(h) => h,
        Err(_) => return (StatusCode::BAD_REQUEST, "bad block hash\n").into_response(),
    };
    let Some(cands) = s.blocks.get(&want) else {
        return (StatusCode::NOT_FOUND, "unknown block\n").into_response();
    };
    let cands = cands.clone(); // Arc clone — O(1)
    match tokio::task::spawn_blocking(move || read_verified_block(&want, &cands)).await {
        Ok(Ok(bytes)) => Response::builder()
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .header(header::CONTENT_LENGTH, bytes.len().to_string())
            .header(header::ETAG, format!("\"{hash_hex}\""))
            .body(Body::from(bytes))
            .unwrap_or_else(|_| {
                (StatusCode::INTERNAL_SERVER_ERROR, "build failed\n").into_response()
            }),
        Ok(Err(e)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("block read failed: {e}\n"),
        )
            .into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "blocking task failed\n").into_response(),
    }
}

fn read_verified_block(want: &[u8; 32], cands: &[Locator]) -> Result<Vec<u8>, String> {
    for loc in cands {
        let f = match std::fs::File::open(&loc.abs) {
            Ok(f) => f,
            Err(_) => continue,
        };
        let mut buf = vec![0u8; loc.len as usize];
        let mut off = 0;
        while off < buf.len() {
            match crate::fs::read_at(&f, &mut buf[off..], loc.offset + off as u64) {
                Ok(0) => break,
                Ok(n) => off += n,
                Err(_) => break,
            }
        }
        if off != buf.len() {
            continue;
        }
        if blake3::hash(&buf).as_bytes() == want {
            return Ok(buf);
        }
    }
    Err("block failed verification (source changed on disk?)".into())
}

fn render_tree(s: &Shared, prefix: &str, q: &HashMap<String, String>) -> Html<String> {
    let qs = pass_query(s.token.as_deref(), q);
    let Kind::Dir { files, .. } = &s.kind else {
        return Html(String::new());
    };

    if !prefix.is_empty() && files.iter().any(|f| f.rel == prefix) {
        return Html(format!(
            "<!doctype html><html><body><a href=\"/__rh__/f/{prefix}{qs}\">download {}</a></body></html>",
            html_escape(prefix)
        ));
    }

    let mut dirs: BTreeMap<String, (usize, u64)> = BTreeMap::new();
    let mut plain: Vec<(&str, u64)> = Vec::new();
    for f in files {
        let rest = if prefix.is_empty() {
            f.rel.as_str()
        } else if let Some(r) = f.rel.strip_prefix(&format!("{prefix}/")) {
            r
        } else {
            continue;
        };
        match rest.split_once('/') {
            Some((d, _)) => {
                let e = dirs.entry(d.to_string()).or_insert((0, 0));
                e.0 += 1;
                e.1 += f.size;
            }
            None => plain.push((rest, f.size)),
        }
    }

    if !prefix.is_empty() && dirs.is_empty() && plain.is_empty() {
        return Html(format!(
            "<!doctype html><html><body><h1>no such directory: {}</h1><p><a href=\"/\">root</a></p></body></html>",
            html_escape(prefix)
        ));
    }

    let mut html = String::new();
    html.push_str("<!doctype html><html><head><meta charset=\"utf-8\">");
    html.push_str("<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">");
    html.push_str("<title>rabbit-hole</title>");
    html.push_str("<style>body{font-family:monospace;max-width:60em;margin:2em auto;padding:0 1em}li{margin:.2em 0}.banner{border:1px dashed #888;padding:.6em;margin-bottom:1em}</style>");
    html.push_str("</head><body>");
    html.push_str("<div class=\"banner\">Full folder sync + verified transfer: install <b>rh</b> and run <code>rh fetch &lt;code-or-URL&gt; &lt;dest&gt;</code></div>");
    html.push_str(&format!(
        "<h1>{}</h1><ul>",
        html_escape(if prefix.is_empty() { "/" } else { prefix })
    ));

    if !prefix.is_empty() {
        let parent = prefix.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
        let up = if parent.is_empty() {
            format!("/__rh__/tree{qs}")
        } else {
            format!("/__rh__/tree/{parent}{qs}")
        };
        html.push_str(&format!("<li><a href=\"{up}\">..</a></li>"));
    }

    for (dir_name, (file_count, bytes)) in &dirs {
        let href = if prefix.is_empty() {
            format!("/__rh__/tree/{dir_name}{qs}")
        } else {
            format!("/__rh__/tree/{prefix}/{dir_name}{qs}")
        };
        html.push_str(&format!(
            "<li>[dir] <a href=\"{href}\">{}</a> ({file_count} files, {bytes} B)</li>",
            html_escape(dir_name)
        ));
    }

    plain.sort();

    for (name, size) in plain {
        let rel = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}/{name}")
        };
        html.push_str(&format!(
            "<li><a href=\"/__rh__/f/{rel}{qs}\">{}</a> ({size} B)</li>",
            html_escape(name)
        ));
    }
    html.push_str("</ul></body></html>");

    Html(html)
}

fn pass_query(token: Option<&str>, q: &HashMap<String, String>) -> String {
    if token.is_some()
        && let Some(t) = q.get("token")
    {
        return format!("?token={}", percent_encode(t));
    }
    String::new()
}

// ---------- auth ----------

// 32 CSPRNG bytes as fixed 64-char hex — fixed width keeps parsing trivial
pub fn generate_token() -> String {
    let mut buf = [0u8; 32];
    rand::rng().fill_bytes(&mut buf);
    crate::protocol::manifest::common::hex_encode(&buf)
}

fn query_ok(q: &HashMap<String, String>, expected: Option<&str>) -> bool {
    let Some(want) = expected else {
        return true;
    };
    q.get("token").is_some_and(|got| ct_eq(got, want))
}

fn ct_eq(a: &str, b: &str) -> bool {
    // TODO: Bench and optimize with SIMD
    let (x, y) = (a.as_bytes(), b.as_bytes());
    if x.len() != y.len() {
        return false;
    }
    let mut d = 0u8;
    for i in 0..x.len() {
        d |= x[i] ^ y[i];
    }
    d == 0
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Bearer")],
        "missing or invalid token\n",
    )
        .into_response()
}

fn bearer_ok(headers: &HeaderMap, q: &HashMap<String, String>, expected: Option<&str>) -> bool {
    let Some(want) = expected else {
        return true;
    };
    if let Some(v) = headers.get(header::AUTHORIZATION)
        && let Ok(s) = v.to_str()
        && let Some(got) = s.strip_prefix("Bearer ")
        && ct_eq(got.trim(), want)
    {
        return true;
    }
    if let Some(got) = q.get("token")
        && ct_eq(got, want)
    {
        return true;
    }
    false
}

// ---------- range ----------

fn percent_encode(s: &str) -> String {
    // TODO: Optimize with fixed capacity vector and inplace append/write to buffer
    let mut o = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                o.push(b as char)
            }
            _ => o.push_str(&format!("%{b:02X}")),
        }
    }
    o
}

fn html_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            _ => o.push(c),
        }
    }
    o
}

struct Wanted {
    start: u64,
    end: u64,
}

fn parse_range(h: Option<&str>, total: u64) -> Result<Option<Wanted>, StatusCode> {
    let Some(h) = h else { return Ok(None) };
    let Some(spec) = h.trim().strip_prefix("bytes=") else {
        return Ok(None);
    };
    let spec = spec.split(',').next().unwrap_or("").trim();
    if spec.is_empty() || total == 0 {
        return if total == 0 && !spec.is_empty() {
            Err(StatusCode::RANGE_NOT_SATISFIABLE)
        } else {
            Ok(None)
        };
    }
    if let Some(suf) = spec.strip_prefix('-') {
        let n: u64 = suf
            .trim()
            .parse()
            .map_err(|_| StatusCode::RANGE_NOT_SATISFIABLE)?;
        if n == 0 {
            return Err(StatusCode::RANGE_NOT_SATISFIABLE);
        }
        let n = n.min(total);
        return Ok(Some(Wanted {
            start: total - n,
            end: total - 1,
        }));
    }
    let (s, e) = spec
        .split_once('-')
        .ok_or(StatusCode::RANGE_NOT_SATISFIABLE)?;
    let start: u64 = s
        .trim()
        .parse()
        .map_err(|_| StatusCode::RANGE_NOT_SATISFIABLE)?;
    if start >= total {
        return Err(StatusCode::RANGE_NOT_SATISFIABLE);
    }
    let end = if e.trim().is_empty() {
        total - 1
    } else {
        e.trim()
            .parse::<u64>()
            .map_err(|_| StatusCode::RANGE_NOT_SATISFIABLE)?
            .min(total - 1)
    };
    if end < start {
        return Err(StatusCode::RANGE_NOT_SATISFIABLE);
    }
    Ok(Some(Wanted { start, end }))
}

async fn stream_file_response(
    abs: PathBuf,
    name: &str,
    total: u64,
    range_h: Option<String>,
    etag: &str,
) -> Response {
    let wanted = match parse_range(range_h.as_deref(), total) {
        Ok(w) => w,
        Err(s) => {
            return (
                s,
                [(header::CONTENT_RANGE, format!("bytes */{total}"))],
                "unsatisfiable range\n",
            )
                .into_response();
        }
    };
    let (start, len) = match wanted {
        Some(w) => (w.start, w.end - w.start + 1),
        None => (0, total),
    };

    let mut f = match tokio::fs::File::open(&abs).await {
        Ok(f) => f,
        Err(_) => return (StatusCode::NOT_FOUND, "file gone from disk\n").into_response(),
    };
    if start > 0 && f.seek(SeekFrom::Start(start)).await.is_err() {
        return (StatusCode::INTERNAL_SERVER_ERROR, "seek failed\n").into_response();
    }

    let body = Body::from_stream(ReaderStream::new(f.take(len)));
    let disp = format!(
        "attachment; filename=\"{}\"; filename*=UTF-8''{}",
        sanitize_filename(name),
        percent_encode(name)
    );

    let mut b = Response::builder()
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_DISPOSITION, disp)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, len.to_string())
        .header(header::ETAG, format!("\"{etag}\""));
    if len != total {
        b = b.status(StatusCode::PARTIAL_CONTENT).header(
            header::CONTENT_RANGE,
            format!("bytes {start}-{}/{total}", start + len - 1),
        );
    }
    b.body(body).unwrap_or_else(|_| {
        (StatusCode::INTERNAL_SERVER_ERROR, "response build failed\n").into_response()
    })
}

fn sanitize_filename(n: &str) -> String {
    n.chars()
        .map(|c| {
            if c == '"' || c == '\\' || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .take(128)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- parse_range ---

    fn parse(h: Option<&str>, total: u64) -> Result<Option<Wanted>, axum::http::StatusCode> {
        parse_range(h, total)
    }

    #[test]
    fn no_range_header_returns_none() {
        assert!(parse(None, 1000).unwrap().is_none());
    }

    #[test]
    fn full_range_returns_none() {
        assert!(parse(Some("bytes=0-999"), 1000).unwrap().is_some());
        let w = parse(Some("bytes=0-999"), 1000).unwrap().unwrap();
        assert_eq!(w.start, 0);
        assert_eq!(w.end, 999);
    }

    #[test]
    fn open_ended_range_reaches_last_byte() {
        let w = parse(Some("bytes=500-"), 1000).unwrap().unwrap();
        assert_eq!(w.start, 500);
        assert_eq!(w.end, 999);
    }

    #[test]
    fn suffix_range_counts_from_end() {
        let w = parse(Some("bytes=-100"), 1000).unwrap().unwrap();
        assert_eq!(w.start, 900);
        assert_eq!(w.end, 999);
    }

    #[test]
    fn start_beyond_file_size_is_unsatisfiable() {
        assert!(parse(Some("bytes=1000-"), 1000).is_err());
    }

    #[test]
    fn end_before_start_is_unsatisfiable() {
        assert!(parse(Some("bytes=500-100"), 1000).is_err());
    }

    #[test]
    fn range_on_empty_file_is_unsatisfiable() {
        assert!(parse(Some("bytes=0-0"), 0).is_err());
    }

    // --- ct_eq ---

    #[test]
    fn identical_strings_are_equal() {
        assert!(ct_eq("abc", "abc"));
    }

    #[test]
    fn same_length_different_content_is_not_equal() {
        assert!(!ct_eq("abc", "abd"));
    }

    #[test]
    fn different_lengths_are_not_equal() {
        assert!(!ct_eq("abc", "abcd"));
    }

    // --- html_escape ---

    #[test]
    fn html_special_chars_are_escaped() {
        assert_eq!(html_escape("<a>&\"b"), "&lt;a&gt;&amp;&quot;b");
    }

    #[test]
    fn plain_text_passes_through_unchanged() {
        assert_eq!(html_escape("hello world"), "hello world");
    }

    // --- sanitize_filename ---

    #[test]
    fn control_chars_become_underscores() {
        assert_eq!(sanitize_filename("foo\x00bar"), "foo_bar");
    }

    #[test]
    fn quotes_and_backslashes_become_underscores() {
        assert_eq!(sanitize_filename("foo\"bar\\baz"), "foo_bar_baz");
    }

    #[test]
    fn normal_filename_passes_through() {
        assert_eq!(sanitize_filename("hello world.txt"), "hello world.txt");
    }

    #[test]
    fn long_filename_is_truncated_to_128_chars() {
        let long = "a".repeat(200);
        assert_eq!(sanitize_filename(&long).len(), 128);
    }
}
