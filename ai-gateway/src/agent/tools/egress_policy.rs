use std::net::{Ipv4Addr, Ipv6Addr};

use crate::config::agent::AgentToolEgressPolicyConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EgressPolicyError {
    #[error("tool target URL must use https")]
    HttpsRequired,
    #[error("tool target URL points to a loopback host")]
    LoopbackBlocked,
    #[error("tool target URL points to a metadata service address")]
    MetadataBlocked,
    #[error("tool target URL points to a link-local address")]
    LinkLocalBlocked,
    #[error("tool target URL points to a private network address")]
    PrivateNetworkBlocked,
    #[error("tool target URL is unsupported")]
    UnsupportedUrl,
}

pub fn validate_target_url(
    url: &str,
    cfg: &AgentToolEgressPolicyConfig,
) -> Result<(), EgressPolicyError> {
    let parsed = url::Url::parse(url).map_err(|_| EgressPolicyError::UnsupportedUrl)?;

    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(EgressPolicyError::UnsupportedUrl);
    }

    let host = parsed.host().ok_or(EgressPolicyError::UnsupportedUrl)?;
    // Domain hosts are checked literally here only for localhost. DNS/IP
    // revalidation and DNS rebinding protection belong in future executor or
    // connect-layer hardening.
    if cfg.block_loopback && is_loopback_host(&host) {
        return Err(EgressPolicyError::LoopbackBlocked);
    }
    if cfg.block_metadata_ip && is_metadata_host(&host) {
        return Err(EgressPolicyError::MetadataBlocked);
    }
    if cfg.block_link_local && is_link_local_host(&host) {
        return Err(EgressPolicyError::LinkLocalBlocked);
    }
    if cfg.block_private_network && is_private_network_host(&host) {
        return Err(EgressPolicyError::PrivateNetworkBlocked);
    }
    if cfg.https_only && parsed.scheme() != "https" {
        return Err(EgressPolicyError::HttpsRequired);
    }

    Ok(())
}

fn is_loopback_host(host: &url::Host<&str>) -> bool {
    match host {
        url::Host::Domain(domain) => domain.eq_ignore_ascii_case("localhost"),
        url::Host::Ipv4(addr) => addr.is_loopback(),
        url::Host::Ipv6(addr) => ipv6_mapped_ipv4(addr)
            .map(|addr| addr.is_loopback())
            .unwrap_or_else(|| addr.is_loopback()),
    }
}

fn is_metadata_host(host: &url::Host<&str>) -> bool {
    match host {
        url::Host::Ipv4(addr) => *addr == Ipv4Addr::new(169, 254, 169, 254),
        url::Host::Ipv6(addr) => ipv6_mapped_ipv4(addr).is_some_and(is_metadata_ipv4),
        url::Host::Domain(_) => false,
    }
}

fn is_link_local_host(host: &url::Host<&str>) -> bool {
    match host {
        url::Host::Ipv4(addr) => is_link_local_ipv4(*addr),
        url::Host::Ipv6(addr) => ipv6_mapped_ipv4(addr)
            .map(is_link_local_ipv4)
            .unwrap_or_else(|| is_link_local_ipv6(*addr)),
        url::Host::Domain(_) => false,
    }
}

fn is_private_network_host(host: &url::Host<&str>) -> bool {
    match host {
        url::Host::Ipv4(addr) => is_private_ipv4(*addr),
        url::Host::Ipv6(addr) => ipv6_mapped_ipv4(addr)
            .map(is_private_ipv4)
            .unwrap_or_else(|| is_unique_local_ipv6(*addr)),
        url::Host::Domain(_) => false,
    }
}

fn is_metadata_ipv4(addr: Ipv4Addr) -> bool {
    addr == Ipv4Addr::new(169, 254, 169, 254)
}

fn is_link_local_ipv4(addr: Ipv4Addr) -> bool {
    let octets = addr.octets();
    octets[0] == 169 && octets[1] == 254
}

fn is_private_ipv4(addr: Ipv4Addr) -> bool {
    let octets = addr.octets();
    octets[0] == 10
        || (octets[0] == 172 && (16..=31).contains(&octets[1]))
        || (octets[0] == 192 && octets[1] == 168)
}

fn is_link_local_ipv6(addr: Ipv6Addr) -> bool {
    (addr.segments()[0] & 0xffc0) == 0xfe80
}

fn is_unique_local_ipv6(addr: Ipv6Addr) -> bool {
    (addr.segments()[0] & 0xfe00) == 0xfc00
}

fn ipv6_mapped_ipv4(addr: &Ipv6Addr) -> Option<Ipv4Addr> {
    addr.to_ipv4_mapped()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::agent::AgentToolEgressPolicyConfig;

    #[test]
    fn rejects_localhost_url() {
        let cfg = AgentToolEgressPolicyConfig::default();

        let error = validate_target_url("https://localhost/tool", &cfg)
            .expect_err("localhost should be blocked");

        assert_eq!(error, EgressPolicyError::LoopbackBlocked);
    }

    #[test]
    fn rejects_non_https_when_https_only() {
        let cfg = AgentToolEgressPolicyConfig::default();

        let error = validate_target_url("http://example.com/tool", &cfg)
            .expect_err("http should be blocked by default");

        assert_eq!(error, EgressPolicyError::HttpsRequired);
    }

    #[test]
    fn rejects_loopback_before_https_requirement() {
        let cfg = AgentToolEgressPolicyConfig::default();

        let error = validate_target_url("http://127.0.0.1:8080/tool", &cfg)
            .expect_err("loopback should be blocked before https policy");

        assert_eq!(error, EgressPolicyError::LoopbackBlocked);
    }

    #[test]
    fn rejects_ipv4_private_networks() {
        let cfg = AgentToolEgressPolicyConfig::default();

        for url in [
            "https://10.0.0.1/tool",
            "https://192.168.1.1/tool",
            "https://172.16.0.1/tool",
        ] {
            let error = validate_target_url(url, &cfg).expect_err("private IPv4 should be blocked");

            assert_eq!(error, EgressPolicyError::PrivateNetworkBlocked);
        }
    }

    #[test]
    fn rejects_ipv4_link_local_addresses() {
        let cfg = AgentToolEgressPolicyConfig::default();

        let error = validate_target_url("https://169.254.1.2/tool", &cfg)
            .expect_err("link-local IPv4 should be blocked");

        assert_eq!(error, EgressPolicyError::LinkLocalBlocked);
    }

    #[test]
    fn rejects_metadata_ip() {
        let cfg = AgentToolEgressPolicyConfig {
            https_only: false,
            ..AgentToolEgressPolicyConfig::default()
        };

        let error = validate_target_url("http://169.254.169.254/latest", &cfg)
            .expect_err("metadata IP should be blocked");

        assert_eq!(error, EgressPolicyError::MetadataBlocked);
    }

    #[test]
    fn rejects_metadata_ip_over_https() {
        let cfg = AgentToolEgressPolicyConfig::default();

        let error = validate_target_url("https://169.254.169.254/latest", &cfg)
            .expect_err("metadata IP should be blocked");

        assert_eq!(error, EgressPolicyError::MetadataBlocked);
    }

    #[test]
    fn rejects_ipv6_link_local_addresses() {
        let cfg = AgentToolEgressPolicyConfig::default();

        let error = validate_target_url("https://[fe80::1]/tool", &cfg)
            .expect_err("link-local IPv6 should be blocked");

        assert_eq!(error, EgressPolicyError::LinkLocalBlocked);
    }

    #[test]
    fn rejects_ipv6_unique_local_addresses() {
        let cfg = AgentToolEgressPolicyConfig::default();

        let error = validate_target_url("https://[fd00::1]/tool", &cfg)
            .expect_err("unique local IPv6 should be blocked");

        assert_eq!(error, EgressPolicyError::PrivateNetworkBlocked);
    }

    #[test]
    fn rejects_ipv4_mapped_ipv6_loopback() {
        let cfg = AgentToolEgressPolicyConfig::default();

        let error = validate_target_url("https://[::ffff:127.0.0.1]/tool", &cfg)
            .expect_err("IPv4-mapped loopback should be blocked");

        assert_eq!(error, EgressPolicyError::LoopbackBlocked);
    }

    #[test]
    fn rejects_ipv4_mapped_ipv6_metadata() {
        let cfg = AgentToolEgressPolicyConfig::default();

        let error = validate_target_url("https://[::ffff:169.254.169.254]/tool", &cfg)
            .expect_err("IPv4-mapped metadata should be blocked");

        assert_eq!(error, EgressPolicyError::MetadataBlocked);
    }

    #[test]
    fn accepts_https_public_host() {
        let cfg = AgentToolEgressPolicyConfig::default();

        validate_target_url("https://api.example.com/tool", &cfg)
            .expect("public https target should be accepted");
    }
}
