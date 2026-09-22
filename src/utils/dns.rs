use std::{net::SocketAddr, time::Duration};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderStatus {
    Pending,
    Ok(usize),
    Failed,
}

impl ProviderStatus {
    pub fn label(&self) -> &'static str {
        match self {
            ProviderStatus::Pending => "pending",
            ProviderStatus::Ok(_) => "ok",
            ProviderStatus::Failed => "failed",
        }
    }
}

pub struct DynamicDnsState {
    pub cloudflare: ProviderStatus,
    pub google: ProviderStatus,
    pub quad9: ProviderStatus,
    pub addrs: Vec<SocketAddr>,
}

pub async fn resolve_all_doh(host: &str) -> DynamicDnsState {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
    {
        Ok(c) => c,
        Err(_) => {
            return DynamicDnsState {
                cloudflare: ProviderStatus::Failed,
                google: ProviderStatus::Failed,
                quad9: ProviderStatus::Failed,
                addrs: Vec::new(),
            };
        }
    };

    let cf_url = format!("https://1.1.1.1/dns-query?name={host}&type=A");
    let google_url = format!("https://8.8.8.8/resolve?name={host}&type=A");

    let wire_query = build_dns_wirequery(host);
    let wire_b64 = base64_url_encode(&wire_query);
    let quad9_url = format!("https://dns.quad9.net/dns-query?dns={wire_b64}");

    let cf_fut = client
        .get(&cf_url)
        .header("Accept", "application/dns-json")
        .send();

    let google_fut = client
        .get(&google_url)
        .header("Accept", "application/json")
        .send();

    let quad9_fut = client
        .get(&quad9_url)
        .header("Accept", "application/dns-message")
        .send();

    // All three providers are fetched concurrently in a single round-trip.
    // IPs are parsed once and reused for both status reporting and addr collection.
    let (cf_res, google_res, quad9_res) = tokio::join!(cf_fut, google_fut, quad9_fut);

    let (cf_status, cf_ips) = match cf_res {
        Ok(r) => match r.text().await {
            Ok(t) => {
                let ips = parse_doh_json_ips(&t);
                let status = if ips.is_empty() {
                    ProviderStatus::Failed
                } else {
                    ProviderStatus::Ok(ips.len())
                };
                (status, ips)
            }
            Err(_) => (ProviderStatus::Failed, Vec::new()),
        },
        Err(_) => (ProviderStatus::Failed, Vec::new()),
    };

    let (google_status, google_ips) = match google_res {
        Ok(r) => match r.text().await {
            Ok(t) => {
                let ips = parse_doh_json_ips(&t);
                let status = if ips.is_empty() {
                    ProviderStatus::Failed
                } else {
                    ProviderStatus::Ok(ips.len())
                };
                (status, ips)
            }
            Err(_) => (ProviderStatus::Failed, Vec::new()),
        },
        Err(_) => (ProviderStatus::Failed, Vec::new()),
    };

    let (quad9_status, quad9_ips) = match quad9_res {
        Ok(r) => match r.bytes().await {
            Ok(b) => {
                let ips = parse_dns_wire_ips(&b);
                let status = if ips.is_empty() {
                    ProviderStatus::Failed
                } else {
                    ProviderStatus::Ok(ips.len())
                };
                (status, ips)
            }
            Err(_) => (ProviderStatus::Failed, Vec::new()),
        },
        Err(_) => (ProviderStatus::Failed, Vec::new()),
    };

    // Deduplicate addrs collected from all three providers — no second HTTP round-trip.
    let mut addrs: Vec<SocketAddr> = Vec::new();
    for ip in cf_ips.into_iter().chain(google_ips).chain(quad9_ips) {
        if !addrs.contains(&ip) {
            addrs.push(ip);
        }
    }

    DynamicDnsState {
        cloudflare: cf_status,
        google: google_status,
        quad9: quad9_status,
        addrs,
    }
}

pub fn parse_doh_json_ips(json: &str) -> Vec<SocketAddr> {
    let mut addrs = Vec::new();
    let mut search = json;
    while let Some(pos) = search.find("\"data\":") {
        search = &search[pos + 7..];
        let search_trimmed = search.trim_start();
        if let Some(start_quote) = search_trimmed.find('"') {
            let rest = &search_trimmed[start_quote + 1..];
            if let Some(end_quote) = rest.find('"') {
                let ip_str = &rest[..end_quote];
                if let Ok(ip) = ip_str.parse::<std::net::IpAddr>() {
                    addrs.push(SocketAddr::new(ip, 443));
                }
            }
        }
    }
    addrs
}

pub fn build_dns_wirequery(host: &str) -> Vec<u8> {
    let mut buf = vec![
        0x00, 0x00, // ID
        0x01, 0x00, // Flags: RD = 1
        0x00, 0x01, // QDCOUNT = 1
        0x00, 0x00, // ANCOUNT = 0
        0x00, 0x00, // NSCOUNT = 0
        0x00, 0x00, // ARCOUNT = 0
    ];
    for part in host.split('.') {
        if !part.is_empty() {
            buf.push(part.len() as u8);
            buf.extend_from_slice(part.as_bytes());
        }
    }
    buf.push(0x00);
    buf.extend_from_slice(&[0x00, 0x01]); // QTYPE = A
    buf.extend_from_slice(&[0x00, 0x01]); // QCLASS = IN
    buf
}

pub fn parse_dns_wire_ips(buf: &[u8]) -> Vec<SocketAddr> {
    let mut addrs = Vec::new();
    if buf.len() < 12 {
        return addrs;
    }
    let anc = u16::from_be_bytes([buf[6], buf[7]]);
    if anc == 0 {
        return addrs;
    }
    let mut idx = 12;
    // skip qname
    while idx < buf.len() {
        let len = buf[idx] as usize;
        if len == 0 {
            idx += 1;
            break;
        }
        if (len & 0xc0) == 0xc0 {
            idx += 2;
            break;
        }
        idx += 1 + len;
    }
    idx += 4; // qtype + qclass

    for _ in 0..anc {
        if idx >= buf.len() {
            break;
        }
        if (buf[idx] & 0xc0) == 0xc0 {
            idx += 2;
        } else {
            while idx < buf.len() && buf[idx] != 0 {
                idx += 1 + buf[idx] as usize;
            }
            idx += 1;
        }
        if idx + 10 > buf.len() {
            break;
        }
        let rtype = u16::from_be_bytes([buf[idx], buf[idx + 1]]);
        let rdlen = u16::from_be_bytes([buf[idx + 8], buf[idx + 9]]) as usize;
        idx += 10;
        if rtype == 1 && rdlen == 4 && idx + 4 <= buf.len() {
            let ip = std::net::Ipv4Addr::new(buf[idx], buf[idx + 1], buf[idx + 2], buf[idx + 3]);
            addrs.push(SocketAddr::new(std::net::IpAddr::V4(ip), 443));
        }
        idx += rdlen;
    }

    addrs
}

fn base64_url_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    let mut i = 0;
    while i < input.len() {
        let b0 = input[i] as u32;
        let b1 = if i + 1 < input.len() {
            input[i + 1] as u32
        } else {
            0
        };
        let b2 = if i + 2 < input.len() {
            input[i + 2] as u32
        } else {
            0
        };

        let triple = (b0 << 16) | (b1 << 8) | b2;

        out.push(ALPHABET[((triple >> 18) & 0x3F) as usize] as char);
        out.push(ALPHABET[((triple >> 12) & 0x3F) as usize] as char);
        if i + 1 < input.len() {
            out.push(ALPHABET[((triple >> 6) & 0x3F) as usize] as char);
        }
        if i + 2 < input.len() {
            out.push(ALPHABET[(triple & 0x3F) as usize] as char);
        }

        i += 3;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_doh_json_ips_extracts_ipv4_addresses() {
        let sample = r#"{"Status":0,"Answer":[{"name":"test.com","type":1,"data":"104.16.230.132"},{"name":"test.com","type":1,"data":"104.16.231.132"}]}"#;
        let ips = parse_doh_json_ips(sample);
        assert_eq!(ips.len(), 2);
        assert_eq!(ips[0], "104.16.230.132:443".parse().unwrap());
        assert_eq!(ips[1], "104.16.231.132:443".parse().unwrap());
    }

    #[test]
    fn wire_query_building_is_valid() {
        let q = build_dns_wirequery("google.com");
        assert_eq!(q[0..2], [0, 0]);
        assert_eq!(q[12], 6); // 'google' length
    }
}
