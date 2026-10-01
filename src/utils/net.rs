use std::{
    net::{IpAddr, Ipv4Addr, UdpSocket},
    time::Duration,
};

// Returns the LAN IP that the kernel would route through to reach the internet.
// Uses a UDP connect trick — no packet is actually sent; the kernel routing
// table is consulted to find the right source address.
pub fn lan_ip() -> IpAddr {
    UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| {
            s.connect("8.8.8.8:80")?;
            s.local_addr()
        })
        .map(|a| a.ip())
        .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST))
}

// Fast connectivity gate: can we open a TCP connection within `timeout`?
// Takes an address so tests can point it at a local listener.
pub async fn can_reach(addr: &str, timeout: Duration) -> bool {
    tokio::time::timeout(timeout, tokio::net::TcpStream::connect(addr))
        .await
        .is_ok_and(|r| r.is_ok())
}

// True when there is a usable internet route. Probes Cloudflare's own
// edge: if we cannot reach it, cloudflared cannot work either. Uses a
// literal IP so broken DNS alone does not read as "offline" (DNS-broken
// but routed networks still fail later, at cloudflared, with its logs).
pub async fn has_internet() -> bool {
    can_reach("1.1.1.1:443", Duration::from_secs(3)).await
}

// Resolves on Ctrl-C (SIGINT) or SIGTERM (unix only).
pub async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = signal(SignalKind::terminate()).expect("install SIGTERM handler");
        tokio::select! {
            _ = ctrl_c => {},
            _ = term.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        ctrl_c.await.ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn can_reach_local_listener() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("local addr").to_string();
        assert!(can_reach(&addr, Duration::from_secs(2)).await);
    }

    #[tokio::test]
    async fn cannot_reach_closed_port() {
        // Bind then drop: the port is momentarily guaranteed closed.
        let port = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback")
            .local_addr()
            .expect("local addr")
            .port();
        assert!(!can_reach(&format!("127.0.0.1:{port}"), Duration::from_secs(2)).await);
    }
}
