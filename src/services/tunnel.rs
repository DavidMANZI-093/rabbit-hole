use std::{
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use tokio::io::{AsyncBufReadExt, BufReader};

use crate::{utils::tty::Dots, warn};

const START_TIMEOUT: Duration = Duration::from_secs(60);
const LIVE_TIMEOUT: Duration = Duration::from_secs(45);

// Why a tunnel never materialized. Typed so callers blame accurately
// instead of printing one misleading message for every failure.
#[derive(Debug)]
enum SpawnError {
    NotInstalled(String),
    NoTunnelUrl(String),
}

pub async fn start(port: u16) -> Option<Tunnel> {
    let status = Dots::start("requesting tunnel");
    match spawn(port).await {
        Ok(t) => {
            t.watch_exit();
            status.message("resolving dns");
            let live = t.wait_until_live().await;
            status.finish();
            if !live {
                warn!("tunnel URL not yet reachable at edge — sharing anyway");
            }
            Some(t)
        }
        Err(SpawnError::NotInstalled(e)) => {
            status.finish();
            warn!("cloudflared not found ({e}) — LAN only (run: rh check)");
            None
        }
        Err(SpawnError::NoTunnelUrl(e)) => {
            status.finish();
            warn!("no tunnel URL ({e}) — LAN only (check your internet connection)");
            None
        }
    }
}

async fn spawn(port: u16) -> Result<Tunnel, SpawnError> {
    if !crate::utils::net::has_internet().await {
        return Err(SpawnError::NoTunnelUrl(
            "no internet route to Cloudflare (1.1.1.1:443 unreachable)".to_string(),
        ));
    }

    let origin = format!("http://127.0.0.1:{port}");

    let mut child = crate::cloudflared::command(&origin)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| SpawnError::NotInstalled(format!("spawning cloudflared: {e}")))?;

    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| SpawnError::NotInstalled("cloudflared stderr not piped".to_string()))?;
    let mut lines = BufReader::new(stderr).lines();

    // Phase 1: wait for subdomain assignment (~5s API round-trip to trycloudflare.com)
    let url = tokio::time::timeout(START_TIMEOUT, async {
        while let Some(line) = lines
            .next_line()
            .await
            .map_err(|e| SpawnError::NoTunnelUrl(format!("reading cloudflared stderr: {e}")))?
        {
            if let Some(u) = sniff_url(&line) {
                return Ok::<String, SpawnError>(u);
            }
        }
        Err(SpawnError::NoTunnelUrl(
            "cloudflared exited before printing a tunnel URL".to_string(),
        ))
    })
    .await
    .map_err(|_| {
        SpawnError::NoTunnelUrl("timed out waiting for cloudflared URL (60s)".to_string())
    })??;

    tokio::spawn(async move { while lines.next_line().await.ok().flatten().is_some() {} });

    Ok(Tunnel {
        child: Arc::new(tokio::sync::Mutex::new(child)),
        dismissed: Arc::new(AtomicBool::new(false)),
        url,
    })
}

pub struct Tunnel {
    child: Arc<tokio::sync::Mutex<tokio::process::Child>>,
    dismissed: Arc<AtomicBool>,
    pub url: String,
}

impl Tunnel {
    pub async fn shutdown(self) {
        self.dismissed.store(true, Ordering::SeqCst);
        let mut child = self.child.lock().await;
        if let Err(e) = child.kill().await {
            warn!("tunnel: kill: {e:#} (already exited?)");
        }
        let _ = child.wait().await;
    }

    async fn wait_until_live(&self) -> bool {
        let probe = format!("{}/__rh__/health", self.url);
        let host = self
            .url
            .strip_prefix("https://")
            .or_else(|| self.url.strip_prefix("http://"))
            .unwrap_or(&self.url)
            .split('/')
            .next()
            .unwrap_or("")
            .to_string();

        let start = tokio::time::Instant::now();
        while start.elapsed() < LIVE_TIMEOUT {
            let mut builder = reqwest::Client::builder().timeout(Duration::from_secs(3));
            if host.ends_with(".trycloudflare.com") {
                let state = crate::utils::dns::resolve_all_doh(&host).await;

                if !state.addrs.is_empty() {
                    builder = builder.resolve_to_addrs(&host, &state.addrs);
                }
            }

            if let Ok(client) = builder.build()
                && let Ok(Ok(r)) =
                    tokio::time::timeout(Duration::from_secs(3), client.get(&probe).send()).await
                && r.status().is_success()
            {
                return true;
            }

            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        false
    }

    fn watch_exit(&self) {
        let child = self.child.clone();
        let dismissed = self.dismissed.clone();
        let url = self.url.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(2)).await;
                if dismissed.load(Ordering::SeqCst) {
                    return;
                }
                let mut c = child.lock().await;
                match c.try_wait() {
                    Ok(Some(status)) => {
                        warn!("tunnel died ({status}): {url}");
                        return;
                    }
                    Ok(None) => {}
                    Err(e) => {
                        warn!("tunnel watch: {e:#}");
                        return;
                    }
                }
            }
        });
    }
}

fn sniff_url(line: &str) -> Option<String> {
    let start = line.find("https://")?;
    let rest = &line[start..];
    let end = rest
        .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == ')')
        .unwrap_or(rest.len());
    let cand = &rest[..end];
    let host = cand.strip_prefix("https://")?;
    if host.ends_with(".trycloudflare.com") && !host.contains('/') && !host.is_empty() {
        let label = host.strip_suffix(".trycloudflare.com").unwrap_or("");
        if !label.is_empty() {
            return Some(cand.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- sniff_url ---

    #[test]
    fn extracts_url_from_cloudflared_box_line() {
        let line = "2026-09-22T06:40:14Z INF |  https://auckland-contacting-ensure-enhanced.trycloudflare.com  |";
        assert_eq!(
            sniff_url(line),
            Some("https://auckland-contacting-ensure-enhanced.trycloudflare.com".to_string())
        );
    }

    #[test]
    fn extracts_url_from_bare_log_line() {
        let line = "Visit it at https://foo-bar-baz.trycloudflare.com and enjoy";
        assert_eq!(
            sniff_url(line),
            Some("https://foo-bar-baz.trycloudflare.com".to_string())
        );
    }

    #[test]
    fn rejects_http_scheme() {
        // cloudflared always uses https; plain http must not be accepted
        let line = "http://foo-bar-baz.trycloudflare.com";
        assert_eq!(sniff_url(line), None);
    }

    #[test]
    fn rejects_non_trycloudflare_domain() {
        let line = "https://example.com";
        assert_eq!(sniff_url(line), None);
    }

    #[test]
    fn rejects_bare_trycloudflare_domain_without_subdomain() {
        // "https://trycloudflare.com" has no label before the suffix — must reject
        let line = "https://trycloudflare.com";
        assert_eq!(sniff_url(line), None);
    }

    #[test]
    fn rejects_url_with_path() {
        // cloudflared prints only the hostname, never a path
        let line = "https://foo.trycloudflare.com/some/path";
        assert_eq!(sniff_url(line), None);
    }

    #[test]
    fn stops_at_whitespace() {
        let line = "https://my-tunnel.trycloudflare.com is now live";
        assert_eq!(
            sniff_url(line),
            Some("https://my-tunnel.trycloudflare.com".to_string())
        );
    }
}
