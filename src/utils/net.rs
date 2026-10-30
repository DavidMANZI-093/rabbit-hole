// Copyright (c) 2026 David MANZI
// SPDX-License-Identifier: MIT

use std::{
    net::{IpAddr, Ipv4Addr, UdpSocket},
    time::Duration,
};

pub fn lan_ip() -> IpAddr {
    UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| {
            s.connect("8.8.8.8:80")?;
            s.local_addr()
        })
        .map(|a| a.ip())
        .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST))
}

pub async fn has_internet() -> bool {
    can_reach("1.1.1.1:443", Duration::from_secs(3)).await
}

pub async fn can_reach(addr: &str, timeout: Duration) -> bool {
    tokio::time::timeout(timeout, tokio::net::TcpStream::connect(addr))
        .await
        .is_ok_and(|r| r.is_ok())
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
