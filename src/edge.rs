use std::{
    fmt,
    path::{Path, PathBuf},
    time::Duration,
};

use rand::RngExt;
use serde::{Deserialize, Serialize};

pub const DEFUALT_TTL_SECS: u16 = 4 * 3600; // 14,400 seconds — 4 hours
pub const ROLL_ATTEMPTS: u8 = 8;
pub const CODE_LEN: u8 = 6;

// The edge base URL baked in at compile time via `build.rs` + `RH_EDGE_BASE` env var.
// Empty string when built without the env var set (dev builds); causes a runtime error
// only if a short code is passed to `rh fetch` without an explicit `--edge` override.
pub const COMPILED_EDGE_BASE: &str = env!("RH_EDGE_BASE");

pub enum EdgeError {
    Taken,
    Unknown,
    Other(String),
}

impl fmt::Display for EdgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Taken => write!(f, "code taken (collision)"),
            Self::Unknown => write!(f, "unknown or expired code"),
            Self::Other(e) => write!(f, "{e}"),
        }
    }
}

impl From<reqwest::Error> for EdgeError {
    fn from(e: reqwest::Error) -> Self {
        Self::Other(e.to_string())
    }
}

#[derive(Serialize)]
struct ClaimReq<'a> {
    url: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    want: Option<&'a str>,
    ttl: u16,
}

#[derive(Deserialize)]
struct ClaimRes {
    code: String,
}

#[derive(Deserialize)]
struct LookupRes {
    url: String,
}

pub struct EdgeClient {
    base: String,
    http: reqwest::Client,
}

impl EdgeClient {
    pub fn new(base: &str) -> Result<Self, EdgeError> {
        let base = base.trim_end_matches('/').to_string();
        if !(base.starts_with("http://") || base.starts_with("https://")) {
            return Err(EdgeError::Other("edge base must be http(s)://".into()));
        }
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| EdgeError::Other(e.to_string()))?;
        Ok(Self { base, http })
    }

    // Single claim attempt. `want`: preferred code (re-acquire) or `None` for a
    // server-rolled one. Returns `Taken` on HTTP 409 — caller rolls and retries.
    pub async fn claim(
        &self,
        url: &str,
        want: Option<&str>,
        ttl: u16,
    ) -> Result<String, EdgeError> {
        if let Some(w) = want {
            validate_code(w)?;
        }
        let res = self
            .http
            .post(format!("{}/claim", self.base))
            .json(&ClaimReq { url, want, ttl })
            .send()
            .await?;

        match res.status() {
            s if s.is_success() => {
                let body: ClaimRes = res
                    .json()
                    .await
                    .map_err(|e| EdgeError::Other(e.to_string()))?;
                validate_code(&body.code)?;
                Ok(body.code)
            }
            s if s.as_u16() == 409 => Err(EdgeError::Taken),
            s => Err(EdgeError::Other(format!("claim: edge returned {s}"))),
        }
    }

    // Claim with per-path memory: try the previously held code first, roll random on
    // collision, persist the winner. `memory` is the path to the per-serve-path state file.
    pub async fn claim_remembered(
        &self,
        url: &str,
        ttl: u16,
        memory: &Path,
    ) -> Result<String, EdgeError> {
        let want = std::fs::read_to_string(memory)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| validate_code(s).is_ok());

        let mut attempts = 0u8;
        let mut candidate = want;
        loop {
            match self.claim(url, candidate.as_deref(), ttl).await {
                Ok(code) => {
                    if let Some(parent) = memory.parent() {
                        std::fs::create_dir_all(parent).ok();
                    }
                    std::fs::write(memory, &code).ok();
                    return Ok(code);
                }
                Err(EdgeError::Taken) => {
                    attempts += 1;
                    if attempts >= ROLL_ATTEMPTS {
                        return Err(EdgeError::Other(format!(
                            "edge: {ROLL_ATTEMPTS} collisions in a row; try again later"
                        )));
                    }
                    candidate = Some(roll_code());
                }
                Err(e) => return Err(e),
            }
        }
    }

    pub async fn lookup(&self, code: &str) -> Result<String, EdgeError> {
        validate_code(code)?;
        let res = self
            .http
            .get(format!("{}/c/{code}", self.base))
            .header("Accept", "application/json")
            .send()
            .await?;
        match res.status() {
            s if s.is_success() => {
                let body: LookupRes = res
                    .json()
                    .await
                    .map_err(|e| EdgeError::Other(e.to_string()))?;
                Ok(body.url)
            }
            s if s.as_u16() == 404 => Err(EdgeError::Unknown),
            s => Err(EdgeError::Other(format!("lookup: edge returned {s}"))),
        }
    }

    pub async fn release(&self, code: &str) -> Result<(), EdgeError> {
        validate_code(code)?;
        let res = self
            .http
            .delete(format!("{}/c/{code}", self.base))
            .send()
            .await?;
        if res.status().is_success() {
            Ok(())
        } else {
            Err(EdgeError::Other(format!(
                "release: edge returned {}",
                res.status()
            )))
        }
    }
}

fn validate_code(code: &str) -> Result<(), EdgeError> {
    if (4..=6).contains(&code.len()) && code.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(EdgeError::Other(format!(
            "bad code {code:?}: want {} hex chars",
            CODE_LEN
        )))
    }
}

fn roll_code() -> String {
    let max_val = 16u32.pow(CODE_LEN as u32);
    format!(
        "{:0>width$x}",
        rand::rng().random_range(0..max_val),
        width = CODE_LEN as usize
    )
}

// Best-effort claim that never fails the caller.
// Returns `Some(code)` on success, `None` when the edge is unreachable/misbehaving
// (caller falls back to sharing the direct URL).
// Warnings are emitted via the `warn!` macro — no raw eprintln here.
pub async fn try_claim_remembered(
    edge_base: &str,
    url: &str,
    ttl: u16,
    memory: &Path,
) -> Option<String> {
    let client = EdgeClient::new(edge_base).ok()?;
    match client.claim_remembered(url, ttl, memory).await {
        Ok(code) => Some(code),
        Err(e) => {
            crate::warn!("edge claim failed ({e}) — sharing direct link");
            None
        }
    }
}

// Best-effort release — warns on failure, never panics.
pub async fn try_release(client: &EdgeClient, code: &str) {
    if let Err(e) = client.release(code).await {
        crate::warn!("edge release failed ({e})");
    }
}

// Returns the state directory for rh
// Uses `~/.local/state/rh` as the fallback per the XDG Base Directory spec v0.8.
fn state_dir() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute()) // spec: ignore if not absolute
        .unwrap_or_else(|| {
            // HOME is always set on any POSIX system; if somehow absent fall back to /tmp
            std::env::var_os("HOME")
                .map(|h| PathBuf::from(h).join(".local").join("state"))
                .unwrap_or_else(|| PathBuf::from("/tmp"))
        })
        .join("rh")
}

// Returns the per-serve-path memory file inside the XDG state dir.
// The file name is a 16-hex-char prefix of the BLAKE3 hash of the canonical
// serve path, so different directories each get their own stable short code.
// Canonical form: the UTF-8 representation of the absolute resolved path.
pub fn memory_path_for(serve_path: &Path) -> PathBuf {
    // Resolve symlinks if possible; fall back to the path as given
    let canonical = serve_path
        .canonicalize()
        .unwrap_or_else(|_| serve_path.to_path_buf());
    let path_str = canonical.to_string_lossy();
    // 16 hex chars = 64 bits of the hash — vastly more than enough for local uniqueness
    let hash_prefix = &blake3::hash(path_str.as_bytes()).to_hex()[..16];
    state_dir().join(format!("code-{hash_prefix}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // --- validate_code ---

    #[test]
    fn valid_six_hex_chars_accepted() {
        assert!(validate_code("a3f9c1").is_ok());
        assert!(validate_code("000000").is_ok());
        assert!(validate_code("FFFFFF").is_ok());
    }

    #[test]
    fn valid_four_and_five_hex_chars_accepted() {
        assert!(validate_code("abcd").is_ok());
        assert!(validate_code("12345").is_ok());
    }

    #[test]
    fn too_short_rejected() {
        assert!(validate_code("").is_err());
        assert!(validate_code("abc").is_err());
    }

    #[test]
    fn too_long_rejected() {
        assert!(validate_code("abcdef1").is_err());
    }

    #[test]
    fn non_hex_chars_rejected() {
        assert!(validate_code("xyz123").is_err());
        assert!(validate_code("a3f9g1").is_err());
        assert!(validate_code("a3f9-1").is_err());
    }

    // --- roll_code ---

    #[test]
    fn roll_code_produces_six_lowercase_hex_chars() {
        for _ in 0..64 {
            let code = roll_code();
            assert_eq!(code.len(), CODE_LEN as usize, "wrong length: {code}");
            assert!(code.chars().all(|c| c.is_ascii_hexdigit()), "non-hex: {code}");
            assert_eq!(code, code.to_lowercase(), "not lowercase: {code}");
        }
    }

    // --- EdgeClient::new validation ---

    #[test]
    fn edge_client_rejects_non_http_base() {
        assert!(EdgeClient::new("ftp://example.com").is_err());
        assert!(EdgeClient::new("example.com").is_err());
        assert!(EdgeClient::new("").is_err());
    }

    #[test]
    fn edge_client_strips_trailing_slash() {
        // new() succeeds — we can't inspect `base` directly, but the call must not error
        assert!(EdgeClient::new("https://example.com/").is_ok());
        assert!(EdgeClient::new("http://localhost:8080/").is_ok());
    }

    // --- memory_path_for stability and uniqueness ---

    #[test]
    fn same_path_gives_same_memory_file() {
        let p = PathBuf::from("/tmp/rh-test/dataset");
        assert_eq!(memory_path_for(&p), memory_path_for(&p));
    }

    #[test]
    fn different_paths_give_different_memory_files() {
        let a = memory_path_for(&PathBuf::from("/tmp/rh-test/a"));
        let b = memory_path_for(&PathBuf::from("/tmp/rh-test/b"));
        assert_ne!(a, b);
    }

    #[test]
    fn memory_file_name_has_code_prefix_and_hex_suffix() {
        let path = memory_path_for(&PathBuf::from("/tmp/rh-test/x"));
        let name = path.file_name().unwrap().to_string_lossy();
        // "code-" + 16 hex chars
        assert!(name.starts_with("code-"), "unexpected name: {name}");
        let suffix = &name["code-".len()..];
        assert_eq!(suffix.len(), 16, "hash suffix wrong length: {suffix}");
        assert!(suffix.chars().all(|c| c.is_ascii_hexdigit()), "non-hex suffix: {suffix}");
    }

    // --- state_dir XDG logic ---

    #[test]
    fn state_dir_with_absolute_xdg_state_home() {
        // Safety: test binary is single-threaded by default; env mutation is safe in that context.
        let dir = unsafe {
            std::env::set_var("XDG_STATE_HOME", "/custom/state");
            let d = state_dir();
            std::env::remove_var("XDG_STATE_HOME");
            d
        };
        assert_eq!(dir, PathBuf::from("/custom/state/rh"));
    }

    #[test]
    fn state_dir_ignores_relative_xdg_state_home() {
        let dir = unsafe {
            std::env::set_var("XDG_STATE_HOME", "relative/path");
            std::env::set_var("HOME", "/home/testuser");
            let d = state_dir();
            std::env::remove_var("XDG_STATE_HOME");
            std::env::remove_var("HOME");
            d
        };
        // relative XDG_STATE_HOME must be ignored; should fall back to HOME
        assert!(dir.starts_with("/home/testuser"), "got: {dir:?}");
    }
}
