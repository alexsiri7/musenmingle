//! Which addresses [`crate::fetch::FetchContext`] may connect to.
//!
//! Sources fetch URLs other people control (links and image URLs from
//! scraped pages, and whatever those redirect to), so only public internet
//! addresses are allowed: never loopback, private, link-local (cloud
//! metadata), CGNAT, unique-local IPv6 (Railway's private network is
//! `fd12::/16`, its names `*.railway.internal`) or other special ranges.
//!
//! [`check_url`] judges a URL before any request and on every redirect hop;
//! [`GuardedResolver`] judges what a host name resolves to, and is the
//! client's only resolver, so the connection uses exactly the addresses it
//! checked (no DNS-rebinding gap).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use url::{Host, Url};

/// Why a URL or host was refused. The message never holds a query string.
#[derive(Debug, thiserror::Error)]
#[error("refused {0}")]
pub struct Blocked(pub String);

fn in_v4(ip: Ipv4Addr, net: [u8; 4], prefix: u32) -> bool {
    let mask = u32::MAX.checked_shl(32 - prefix).unwrap_or(0);
    u32::from(ip) & mask == u32::from(Ipv4Addr::from(net)) & mask
}

fn in_v6(ip: Ipv6Addr, net: Ipv6Addr, prefix: u32) -> bool {
    let mask = u128::MAX.checked_shl(128 - prefix).unwrap_or(0);
    u128::from(ip) & mask == u128::from(net) & mask
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    const REFUSED: [([u8; 4], u32); 14] = [
        ([0, 0, 0, 0], 8),
        ([10, 0, 0, 0], 8),
        ([100, 64, 0, 0], 10),
        ([127, 0, 0, 0], 8),
        ([169, 254, 0, 0], 16),
        ([172, 16, 0, 0], 12),
        ([192, 0, 0, 0], 24),
        ([192, 0, 2, 0], 24),
        ([192, 168, 0, 0], 16),
        ([198, 18, 0, 0], 15),
        ([198, 51, 100, 0], 24),
        ([203, 0, 113, 0], 24),
        ([224, 0, 0, 0], 4),
        ([240, 0, 0, 0], 4),
    ];
    !REFUSED.iter().any(|&(net, prefix)| in_v4(ip, net, prefix))
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    let embedded_v4 = |ip: Ipv6Addr| {
        let [.., a, b, c, d] = ip.octets();
        Ipv4Addr::new(a, b, c, d)
    };
    if in_v6(ip, Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0, 0), 96)
        || in_v6(ip, Ipv6Addr::new(0x64, 0xff9b, 0, 0, 0, 0, 0, 0), 96)
    {
        return is_public_v4(embedded_v4(ip));
    }
    const REFUSED: [(Ipv6Addr, u32); 10] = [
        // `::`, `::1` and the deprecated IPv4-compatible addresses.
        (Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 0), 96),
        (Ipv6Addr::new(0x100, 0, 0, 0, 0, 0, 0, 0), 64),
        (Ipv6Addr::new(0x2001, 0, 0, 0, 0, 0, 0, 0), 32),
        (Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 0), 32),
        (Ipv6Addr::new(0x2002, 0, 0, 0, 0, 0, 0, 0), 16),
        (Ipv6Addr::new(0xfc00, 0, 0, 0, 0, 0, 0, 0), 7),
        (Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 0), 10),
        (Ipv6Addr::new(0xfec0, 0, 0, 0, 0, 0, 0, 0), 10),
        (Ipv6Addr::new(0xff00, 0, 0, 0, 0, 0, 0, 0), 8),
        // NAT64's local-use prefix.
        (Ipv6Addr::new(0x64, 0xff9b, 1, 0, 0, 0, 0, 0), 48),
    ];
    !REFUSED.iter().any(|&(net, prefix)| in_v6(ip, net, prefix))
}

/// Whether `ip` is a public internet address (`IpAddr::is_global` is
/// unstable, so the special ranges are listed here).
pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => is_public_v6(v6),
    }
}

/// Host names that only mean something on a private network.
pub fn is_blocked_host_name(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    host == "localhost"
        || [".localhost", ".local", ".internal"]
            .iter()
            .any(|suffix| host.ends_with(suffix))
}

fn allowed_ip(ip: IpAddr, allow_loopback: bool) -> bool {
    is_public_ip(ip) || (allow_loopback && ip.is_loopback())
}

/// Scheme and host rules for a URL we are about to fetch: http(s) only, no
/// private host names, and IP-literal hosts must be public (the HTTP
/// connector does not ask the resolver about literals, so this is their
/// only check). `allow_loopback` is for tests against local mock servers.
pub fn check_url(url: &Url, allow_loopback: bool) -> Result<(), Blocked> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(Blocked(format!("scheme {}", url.scheme())));
    }
    let ip = match url.host() {
        None => return Err(Blocked("URL without a host".into())),
        Some(Host::Domain(name)) if is_blocked_host_name(name) => {
            return Err(Blocked(format!("host name {name}")));
        }
        Some(Host::Domain(_)) => return Ok(()),
        Some(Host::Ipv4(ip)) => IpAddr::V4(ip),
        Some(Host::Ipv6(ip)) => IpAddr::V6(ip),
    };
    if allowed_ip(ip, allow_loopback) {
        Ok(())
    } else {
        Err(Blocked(format!("non-public address {ip}")))
    }
}

/// The DNS resolver of `FetchContext`'s client. A name is refused outright
/// if any of its addresses is not public: a public name that also points
/// inside a private network is not one we want to reach.
#[derive(Debug, Clone, Copy)]
pub struct GuardedResolver {
    pub allow_loopback: bool,
}

impl Resolve for GuardedResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let allow_loopback = self.allow_loopback;
        Box::pin(async move {
            let host = name.as_str();
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, 0)).await?.collect();
            if let Some(bad) = addrs.iter().find(|a| !allowed_ip(a.ip(), allow_loopback)) {
                return Err(Blocked(format!(
                    "{host}, which resolves to non-public address {}",
                    bad.ip()
                ))
                .into());
            }
            Ok(Box::new(addrs.into_iter()) as Addrs)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_and_non_public_ips() {
        let refused = [
            "0.1.2.3",
            "10.0.0.1",
            "100.64.0.1",
            "100.127.255.254",
            "127.0.0.1",
            "169.254.169.254",
            "172.16.0.1",
            "172.31.255.255",
            "192.0.0.8",
            "192.0.2.1",
            "192.168.1.1",
            "198.18.0.1",
            "198.19.255.1",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "240.0.0.1",
            "255.255.255.255",
            "::",
            "::1",
            "::10.0.0.1",
            "::ffff:10.0.0.1",
            "::ffff:127.0.0.1",
            "64:ff9b::a9fe:a9fe",
            "64:ff9b:1::1",
            "100::1",
            "2001::1",
            "2001:db8::1",
            "2002::1",
            "fc00::1",
            "fd12::1",
            "fe80::1",
            "fec0::1",
            "ff02::1",
        ];
        for ip in refused {
            assert!(!is_public_ip(ip.parse().unwrap()), "{ip} should be refused");
        }
        let allowed = [
            "8.8.8.8",
            "151.101.1.1",
            "100.63.255.255",
            "100.128.0.1",
            "172.32.0.1",
            "2a04:4e42::1",
            "::ffff:8.8.8.8",
            "64:ff9b::808:808",
        ];
        for ip in allowed {
            assert!(is_public_ip(ip.parse().unwrap()), "{ip} should be allowed");
        }
    }

    #[test]
    fn blocked_host_names() {
        for host in [
            "localhost",
            "LOCALHOST.",
            "a.localhost",
            "printer.local",
            "api.railway.internal",
        ] {
            assert!(is_blocked_host_name(host), "{host} should be refused");
        }
        for host in [
            "example.org",
            "internal.example.org",
            "localhost.example.org",
        ] {
            assert!(!is_blocked_host_name(host), "{host} should be allowed");
        }
    }

    #[test]
    fn check_url_rules() {
        let check = |s: &str, loopback| check_url(&Url::parse(s).unwrap(), loopback);
        for url in [
            "ftp://example.org/",
            "file:///etc/passwd",
            "http://10.0.0.1/",
            "http://[::1]/",
            "http://169.254.169.254/latest/meta-data/",
            "http://api.railway.internal/",
            "http://localhost:8080/",
        ] {
            assert!(check(url, false).is_err(), "{url} should be refused");
        }
        assert!(check("https://example.org/a?key=1", false).is_ok());
        assert!(check("http://8.8.8.8/", false).is_ok());
        assert!(check("http://127.0.0.1:1/", false).is_err());
        assert!(check("http://127.0.0.1:1/", true).is_ok());
        assert!(check("http://10.0.0.1/", true).is_err());
        assert!(check("http://localhost:1/", true).is_err());
    }

    #[tokio::test]
    async fn resolver_refuses_names_that_resolve_to_loopback() {
        let name = || "localhost".parse::<Name>().unwrap();
        let err = GuardedResolver {
            allow_loopback: false,
        }
        .resolve(name())
        .await
        .err()
        .expect("localhost refused");
        assert!(err.downcast_ref::<Blocked>().is_some(), "{err}");
        assert!(
            GuardedResolver {
                allow_loopback: true
            }
            .resolve(name())
            .await
            .is_ok()
        );
    }
}
