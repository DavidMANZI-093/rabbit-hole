
use std::{
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use tokio::io::{AsyncBufReadExt, BufReader};

use crate::warn;

const START_TIMEOUT: Duration = Duration::from_secs(60);
const LIVE_TIMEOUT: Duration = Duration::from_secs(45);

pub async fn spawn(port: u16) -> Result<Tunnel, String> {
    let url = format!("http://127.0.0.1:{port}");
    let mut cmd = tokio::process::Command::new("cloudflared");

    cmd.args(["tunnel", "--no-autoupdate", "--url", &url]);

    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("spawning `cloudflared`: {e}"))?;

    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "cloudflared stderr not piped".to_string())?;
    let mut lines = BufReader::new(stderr).lines();

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

    pub async fn wait_until_live(&self, client: &reqwest::Client) -> bool {
        let probe = format!("{}/__rh__/health", self.url);
        let start = tokio::time::Instant::now();
        while start.elapsed() < LIVE_TIMEOUT {
            match tokio::time::timeout(Duration::from_secs(5), client.get(&probe).send()).await {
                Ok(Ok(r)) if r.status().is_success() => return true,
                _ => tokio::time::sleep(Duration::from_secs(1)).await,
            }
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
