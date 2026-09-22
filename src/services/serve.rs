use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use axum::{
    Router,
    body::Body,
    extract::{Path as UrlPath, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use rand::Rng;

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
        .route("/__rh__/manifest", get(h_manifest))
        .route("/__rh__/block/{:hash}", get(h_block))
        .route("/__rh__/health", get(|| async { "ok" }))
        .with_state(shared)
}

// ---------- handlers ----------

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

// ---------- auth ----------

pub fn generate_token() -> String {
    let mut buf = [0u8; 32];
    rand::rng().fill_bytes(&mut buf);
    crate::protocol::manifest::common::hex_encode(&buf)
}

fn ct_eq(a: &str, b: &str) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
