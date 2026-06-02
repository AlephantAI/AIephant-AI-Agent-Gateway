//! Trusted proxy CIDR + `X-Forwarded-For` parsing (global rate limit).

use std::net::IpAddr;

use http::HeaderMap;
use ipnetwork::IpNetwork;

use crate::error::init::InitError;

/// Parse CIDR strings from config into [`IpNetwork`]; startup fails if any
/// entry is invalid.
pub fn parse_trusted_proxy_networks(
    cidrs: &[String],
) -> Result<Vec<IpNetwork>, InitError> {
    cidrs
        .iter()
        .map(|s| {
            s.parse::<IpNetwork>().map_err(|e| {
                InitError::InvalidClientIpRateLimitConfig(format!(
                    "trusted-proxy-cidrs: invalid CIDR {s:?}: {e}"
                ))
            })
        })
        .collect()
}

/// Return the **client IP** used for rate-limit buckets (design: trust XFF only
/// when peer IP matches trusted CIDRs).
#[must_use]
pub fn effective_client_ip(
    peer_ip: IpAddr,
    headers: &HeaderMap,
    trusted: &[IpNetwork],
) -> IpAddr {
    let trust_xff = trusted.iter().any(|n| n.contains(peer_ip));
    if trust_xff
        && let Some(raw) =
            headers.get("x-forwarded-for").and_then(|h| h.to_str().ok())
        && let Some(first) = raw.split(',').next()
    {
        let trimmed = first.trim();
        if let Ok(ip) = trimmed.parse::<IpAddr>() {
            return ip;
        }
    }
    peer_ip
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use http::HeaderMap;

    use super::*;

    #[test]
    fn empty_trusted_ignores_xff() {
        let peer = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1));
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            http::HeaderValue::from_static("9.9.9.9"),
        );
        assert_eq!(
            effective_client_ip(peer, &headers, &[]),
            peer,
            "without trusted CIDRs, X-Forwarded-For must be ignored"
        );
    }

    #[test]
    fn trusted_peer_uses_xff_leftmost() {
        let peer = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let trusted: Vec<IpNetwork> = vec!["10.0.0.0/8".parse().unwrap()];
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            http::HeaderValue::from_static("203.0.113.5, 10.0.0.2"),
        );
        assert_eq!(
            effective_client_ip(peer, &headers, trusted.as_slice()),
            "203.0.113.5".parse::<IpAddr>().unwrap()
        );
    }

    #[test]
    fn untrusted_peer_ignores_xff() {
        let peer = IpAddr::V4(Ipv4Addr::new(198, 51, 100, 1));
        let trusted: Vec<IpNetwork> = vec!["10.0.0.0/8".parse().unwrap()];
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            http::HeaderValue::from_static("9.9.9.9"),
        );
        assert_eq!(
            effective_client_ip(peer, &headers, trusted.as_slice()),
            peer
        );
    }

    #[test]
    fn invalid_xff_falls_back_to_peer() {
        let peer = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let trusted: Vec<IpNetwork> = vec!["10.0.0.0/8".parse().unwrap()];
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            http::HeaderValue::from_static("not-an-ip"),
        );
        assert_eq!(
            effective_client_ip(peer, &headers, trusted.as_slice()),
            peer
        );
    }

    #[test]
    fn parse_trusted_rejects_garbage() {
        assert!(
            parse_trusted_proxy_networks(&["not-a-cidr".to_string()]).is_err()
        );
    }
}
