use std::{
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use tokio::io::{AsyncBufReadExt, BufReader};

use crate::{utils::ui::prettify, warn};

const START_TIMEOUT: Duration = Duration::from_secs(60);
const LIVE_TIMEOUT: Duration = Duration::from_secs(45);

// the "Registered tunnel connection ... protocol=quic" line appears within milliseconds
// of the URL line — 600ms is generous but cheap to wait
const PROTOCOL_PEEK: Duration = Duration::from_millis(600);

pub async fn spawn(port: u16) -> Result<Tunnel, String> {
    let origin = format!("http://127.0.0.1:{port}");

    // `--no-prechecks` needs cloudflared >= MIN (TUN-10387, 2026.5.0).
    // Older binaries get the compat arg set (prechecks cost ~6s but WAN works).
    // Unknown versions get full args; can't punish what we can't classify.
    let mut compat = false;
    if let crate::cloudflared::Verdict::TooOld(v) = crate::cloudflared::probe() {
        crate::warn!(
            "cloudflared {v} predates minimum {} (tunnel needs --no-prechecks); upgrade cloudflared — running with compat flags",
            crate::cloudflared::MIN
        );
        compat = true;
    }

    let mut cmd = tokio::process::Command::new(crate::cloudflared::binary());
    cmd.arg("tunnel").arg("--no-autoupdate");
    if !compat {
        cmd.arg("--no-prechecks"); // skip post-connection diagnostic table (~6s wasted otherwise)
    }
    cmd.args([
        "--metrics",
        "localhost:0", // random port — avoids conflicts with existing instances
        "--url",
        &origin,
    ]);

    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("spawning cloudflared: {e}"))?;

    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "cloudflared stderr not piped".to_string())?;
    let mut lines = BufReader::new(stderr).lines();

    // Phase 1: wait for subdomain assignment (~5s API round-trip to trycloudflare.com)
    let url = tokio::time::timeout(START_TIMEOUT, async {
        while let Some(line) = lines
            .next_line()
            .await
            .map_err(|e| format!("reading cloudflared stderr: {e}"))?
        {
            if let Some(u) = sniff_url(&line) {
                return Ok::<String, String>(u);
            }
        }
        Err("cloudflared exited before printing a tunnel URL".to_string())
    })
    .await
    .map_err(|_| "timed out waiting for cloudflared URL (60s)".to_string())??;

    // Phase 2: peek briefly for the protocol log line which follows immediately
    // e.g. "Registered tunnel connection connIndex=0 ... protocol=quic"
    let protocol: Option<String> = tokio::time::timeout(PROTOCOL_PEEK, async {
        while let Ok(Some(line)) = lines.next_line().await {
            if let Some(p) = sniff_protocol(&line) {
                return Some(p);
            }
        }
        None
    })
    .await
    .ok()
    .flatten();

    tokio::spawn(async move { while lines.next_line().await.ok().flatten().is_some() {} });

    Ok(Tunnel {
        child: Arc::new(tokio::sync::Mutex::new(child)),
        dismissed: Arc::new(AtomicBool::new(false)),
        url,
        protocol,
    })
}

pub struct Tunnel {
    child: Arc<tokio::sync::Mutex<tokio::process::Child>>,
    dismissed: Arc<AtomicBool>,
    pub url: String,
    pub protocol: Option<String>,
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

    pub async fn wait_until_live<F>(&self, on_status: F) -> bool
    where
        F: Fn(&str),
    {
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
                let msg = format!(
                    "propagating... [cf: {}  google: {}  quad9: {}]",
                    prettify(state.cloudflare.label()),
                    prettify(state.google.label()),
                    prettify(state.quad9.label())
                );
                on_status(&msg);

                if !state.addrs.is_empty() {
                    builder = builder.resolve_to_addrs(&host, &state.addrs);
                }
            } else {
                on_status("activating...");
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

    pub fn watch_exit(&self) {
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

fn sniff_protocol(line: &str) -> Option<String> {
    // matches "protocol=quic" or "protocol=http2" in cloudflared log lines
    let pos = line.find("protocol=")?;
    let rest = &line[pos + "protocol=".len()..];
    let end = rest
        .find(|c: char| c.is_whitespace() || c == '"' || c == ',')
        .unwrap_or(rest.len());
    let proto = &rest[..end];
    matches!(proto, "quic" | "http2" | "http3").then(|| proto.to_string())
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

    // --- sniff_protocol ---

    #[test]
    fn extracts_quic_protocol() {
        let line = "2026-09-22T06:40:16Z INF Registered tunnel connection connIndex=0 connection=abc event=0 ip=1.2.3.4 location=nbo04 protocol=quic";
        assert_eq!(sniff_protocol(line), Some("quic".to_string()));
    }

    #[test]
    fn extracts_http2_protocol() {
        let line = "Registered tunnel connection protocol=http2 connIndex=0";
        assert_eq!(sniff_protocol(line), Some("http2".to_string()));
    }

    #[test]
    fn rejects_unknown_protocol_value() {
        let line = "protocol=http1";
        assert_eq!(sniff_protocol(line), None);
    }

    #[test]
    fn returns_none_for_line_without_protocol_key() {
        let line = "Registered tunnel connection connIndex=0";
        assert_eq!(sniff_protocol(line), None);
    }

    #[test]
    fn stops_at_space_after_protocol_value() {
        let line = "protocol=quic connIndex=0";
        assert_eq!(sniff_protocol(line), Some("quic".to_string()));
    }
}
