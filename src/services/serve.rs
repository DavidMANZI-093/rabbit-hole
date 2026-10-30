// Copyright (c) 2026 David MANZI
// SPDX-License-Identifier: MIT

use std::{collections::HashMap, io, sync::Arc, time::Duration};

use axum::{
    Router,
    body::Body,
    extract::{Path as UrlPath, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};

use crate::{
    services::share::{BlockError, Shared},
    utils::signal::shutdown_signal,
};

const GRACE: Duration = Duration::from_secs(5);

// ---------- lifecycle ----------

pub struct Server {
    port: u16,
    stop: Option<oneshot::Sender<()>>,
    handle: JoinHandle<io::Result<()>>,
}

impl Server {
    pub async fn bind(shared: Arc<Shared>) -> Result<Self, String> {
        let app = router(shared);
        let listener = TcpListener::bind(("0.0.0.0", 0u16))
            .await
            .map_err(|e| format!("bind: {e}"))?;
        let port = listener
            .local_addr()
            .map_err(|e| format!("local addr: {e}"))?
            .port();

        let (stop, stopped) = oneshot::channel::<()>();
        let handle = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    stopped.await.ok();
                })
                .await
        });

        Ok(Self {
            port,
            stop: Some(stop),
            handle,
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    // Waits for Ctrl-C/SIGTERM or an early server exit, then drains for
    // `GRACE` and aborts if the server is still running.
    pub async fn run_until_shutdown(mut self) -> Result<(), String> {
        let early = tokio::select! {
            _ = shutdown_signal() => None,
            r = &mut self.handle => Some(r),
        };
        if let Some(r) = early {
            return join(r);
        }

        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        match tokio::time::timeout(GRACE, &mut self.handle).await {
            Ok(r) => join(r),
            Err(_) => {
                self.handle.abort();
                let _ = (&mut self.handle).await;
                Err(format!(
                    "server did not stop within {}s; aborted",
                    GRACE.as_secs()
                ))
            }
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

fn join(r: Result<io::Result<()>, tokio::task::JoinError>) -> Result<(), String> {
    match r {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(format!("server error: {e}")),
        Err(e) => Err(format!("server task panicked: {e}")),
    }
}

// ---------- routing ----------

fn router(shared: Arc<Shared>) -> Router {
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
    match tokio::task::spawn_blocking(move || s.read_block(&want)).await {
        Ok(Ok(bytes)) => Response::builder()
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .header(header::CONTENT_LENGTH, bytes.len().to_string())
            .header(header::ETAG, format!("\"{hash_hex}\""))
            .body(Body::from(bytes))
            .unwrap_or_else(|_| {
                (StatusCode::INTERNAL_SERVER_ERROR, "build failed\n").into_response()
            }),
        Ok(Err(BlockError::Unknown)) => (StatusCode::NOT_FOUND, "unknown block\n").into_response(),
        Ok(Err(BlockError::Failed(e))) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("block read failed: {e}\n"),
        )
            .into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "blocking task failed\n").into_response(),
    }
}

// ---------- auth ----------

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
