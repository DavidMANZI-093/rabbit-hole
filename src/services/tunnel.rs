// Copyright (c) 2026 David MANZI
// SPDX-License-Identifier: MIT

use std::{
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use tokio::io::{AsyncBufReadExt, BufReader};

use crate::{
    services::cloudflared,
    utils::{self, dns::DynamicDnsState, tty::Dots},
    warn,
};

const START_TIMEOUT: Duration = Duration::from_secs(60);
const LIVE_TIMEOUT: Duration = Duration::from_secs(45);
const DNS_REFRESH: Duration = Duration::from_secs(10);

pub struct Tunnel {
    child: Arc<tokio::sync::Mutex<tokio::process::Child>>,
    dismissed: Arc<AtomicBool>,
    pub url: String,
    host: String,
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

    pub async fn wait_until_live(&self) -> bool {
        let status = Dots::start("resolving dns");
        let live: bool = self.probe_until_live().await;
        status.finish();
        if !live {
            warn!("tunnel URL not resolvable yet, sharing anyway");
        }
        live
    }

    async fn probe_until_live(&self) -> bool {
        let probe = format!("{}/__rh__/health", self.url);
        let host = self.host.as_str();

        let start = tokio::time::Instant::now();
        let mut client: Option<reqwest::Client> = probe_client(host).await;
        let mut refresh_at = tokio::time::Instant::now() + DNS_REFRESH;

        while start.elapsed() < LIVE_TIMEOUT {
            if let Some(c) = client.as_ref()
                && let Ok(Ok(r)) =
                    tokio::time::timeout(Duration::from_secs(3), c.get(&probe).send()).await
                && r.status().is_success()
            {
                return true;
            }

            let now = tokio::time::Instant::now();
            if now >= refresh_at {
                client = probe_client(host).await;
                refresh_at = now + DNS_REFRESH;
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
            status.finish();
            Some(t)
        }
        Err(SpawnError::NotInstalled(e)) => {
            status.finish();
            warn!("cloudflared not found. LAN only (run: rh check). {}", e);
            None
        }
        Err(SpawnError::NoTunnelUrl(e)) => {
            status.finish();
            warn!(
                "no tunnel URL. LAN only (check your internet connection). {}",
                e
            );
            None
        }
    }
}

async fn spawn(port: u16) -> Result<Tunnel, SpawnError> {
    if !utils::net::has_internet().await {
        return Err(SpawnError::NoTunnelUrl(
            "no internet connection. (1.1.1.1:443 unreachable)".to_string(),
        ));
    }

    let origin = format!("http://127.0.0.1:{port}");
    let mut child = cloudflared::command(&origin)
        .await
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| SpawnError::NotInstalled(format!("spawning cloudflared: {}", e)))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| SpawnError::NotInstalled("cloudflared stderr not piped".to_string()))?;
    let mut lines = BufReader::new(stderr).lines();

    let sniffed: SniffedUrl = tokio::time::timeout(START_TIMEOUT, async {
        while let Some(line) = lines
            .next_line()
            .await
            .map_err(|e| SpawnError::NoTunnelUrl(format!("reading cloudflared stderr: {e}")))?
        {
            if let Some(u) = sniff_url(&line) {
                return Ok::<SniffedUrl, SpawnError>(u);
            }
        }
        Err(SpawnError::NoTunnelUrl(
            "cloudflared exited before printing a tunnel URL".to_string(),
        ))
    })
    .await
    .map_err(|_| {
        SpawnError::NoTunnelUrl(format!(
            "timed out waiting for cloudflared URL ({})",
            START_TIMEOUT.as_secs()
        ))
    })??;

    // Gracefully exaust stderr pipe
    tokio::spawn(async move { while lines.next_line().await.ok().flatten().is_some() {} });

    Ok(Tunnel {
        child: Arc::new(tokio::sync::Mutex::new(child)),
        dismissed: Arc::new(AtomicBool::new(false)),
        url: sniffed.url,
        host: sniffed.host,
    })
}

#[derive(Debug, PartialEq, Eq)]
struct SniffedUrl {
    url: String,
    host: String,
}

fn sniff_url(line: &str) -> Option<SniffedUrl> {
    // start: "https://"
    let start: usize = line.find("https://")?;
    let rest: &str = &line[start..];

    // end: index of first probable 'url' termination indicator: ", ', or )
    let end: usize = rest
        .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == ')')
        .unwrap_or(rest.len());
    let cand: &str = &rest[..end];
    let host: &str = cand.strip_prefix("https://")?;

    // return 'cand' & 'host' if ends with ".trycloudflare.com", is-not pointing elsewhere (contains '/').
    // Otherwise, 'None'.
    if host.ends_with(".trycloudflare.com") && !host.contains('/') && !host.is_empty() {
        let label = host.strip_suffix(".trycloudflare.com").unwrap_or("");
        if !label.is_empty() {
            return Some(SniffedUrl {
                url: cand.to_string(),
                host: host.to_string(),
            });
        }
    }
    None
}

async fn probe_client(host: &str) -> Option<reqwest::Client> {
    let mut builder = reqwest::Client::builder().timeout(Duration::from_secs(3));
    if host.ends_with(".trycloudflare.com") {
        let state: DynamicDnsState = utils::dns::resolve_all_doh(host).await;
        if !state.addrs.is_empty() {
            builder = builder.resolve_to_addrs(host, &state.addrs);
        }
    }
    builder.build().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_url_from_cloudflared_box_line() {
        let line = "2026-09-22T06:40:14Z INF |  https://auckland-contacting-ensure-enhanced.trycloudflare.com  |";
        let sniffed = sniff_url(line).expect("sniff url");
        assert_eq!(
            sniffed.url,
            "https://auckland-contacting-ensure-enhanced.trycloudflare.com"
        );
        assert_eq!(
            sniffed.host,
            "auckland-contacting-ensure-enhanced.trycloudflare.com"
        );
    }

    #[test]
    fn extracts_url_from_bare_log_line() {
        let line = "Visit it at https://foo-bar-baz.trycloudflare.com and enjoy";
        let sniffed = sniff_url(line).expect("sniff url");
        assert_eq!(sniffed.url, "https://foo-bar-baz.trycloudflare.com");
        assert_eq!(sniffed.host, "foo-bar-baz.trycloudflare.com");
    }

    #[test]
    fn rejects_http_scheme() {
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
        let line = "https://trycloudflare.com";
        assert_eq!(sniff_url(line), None);
    }

    #[test]
    fn rejects_url_with_path() {
        let line = "https://foo.trycloudflare.com/some/path";
        assert_eq!(sniff_url(line), None);
    }

    #[test]
    fn stops_at_whitespace() {
        let line = "https://my-tunnel.trycloudflare.com is now live";
        let sniffed = sniff_url(line).expect("sniff url");
        assert_eq!(sniffed.url, "https://my-tunnel.trycloudflare.com");
        assert_eq!(sniffed.host, "my-tunnel.trycloudflare.com");
    }
}
