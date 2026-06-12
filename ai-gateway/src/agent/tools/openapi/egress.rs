use crate::{
    agent::tools::{
        egress_policy::validate_target_url,
        openapi::{mapping::OpenApiRequestPlan, types::RuntimeOpenApiTarget},
    },
    config::agent::AgentToolEgressPolicyConfig,
};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OpenApiEgressError {
    #[error("OpenAPI request URL scheme does not match the runtime snapshot")]
    SchemeMismatch,
    #[error("OpenAPI request URL host does not match the runtime snapshot")]
    HostMismatch,
    #[error("OpenAPI request URL port does not match the runtime snapshot")]
    PortMismatch,
    #[error("OpenAPI request URL must not contain username or password")]
    UserInfoForbidden,
    #[error("OpenAPI request URL must not contain a fragment")]
    FragmentForbidden,
    #[error("OpenAPI request URL host is missing")]
    MissingHost,
    #[error("OpenAPI request URL port is missing")]
    MissingPort,
    #[error("OpenAPI request URL points to a known metadata service host")]
    MetadataHostBlocked,
    #[error("OpenAPI request URL is blocked by egress policy")]
    EgressPolicyBlocked,
}

pub fn validate_openapi_egress(
    target: &RuntimeOpenApiTarget,
    plan: &OpenApiRequestPlan,
    policy: &AgentToolEgressPolicyConfig,
) -> Result<(), OpenApiEgressError> {
    if plan.url.scheme() != target.allowed_scheme {
        return Err(OpenApiEgressError::SchemeMismatch);
    }
    if !plan.url.username().is_empty() || plan.url.password().is_some() {
        return Err(OpenApiEgressError::UserInfoForbidden);
    }
    if plan.url.fragment().is_some() {
        return Err(OpenApiEgressError::FragmentForbidden);
    }

    let host = plan.url.host_str().ok_or(OpenApiEgressError::MissingHost)?;
    if !host.eq_ignore_ascii_case(&target.canonical_host) {
        return Err(OpenApiEgressError::HostMismatch);
    }

    let port = plan
        .url
        .port_or_known_default()
        .ok_or(OpenApiEgressError::MissingPort)?;
    if port != target.allowed_port {
        return Err(OpenApiEgressError::PortMismatch);
    }

    if is_known_metadata_hostname(host) {
        return Err(OpenApiEgressError::MetadataHostBlocked);
    }

    // P0 intentionally reuses the literal host checks from the shared egress
    // policy. DNS and rebinding protection belong in the future network
    // executor/connect layer before arbitrary customer hosts are enabled.
    validate_target_url(plan.url.as_str(), policy)
        .map_err(|_| OpenApiEgressError::EgressPolicyBlocked)?;

    Ok(())
}

fn is_known_metadata_hostname(host: &str) -> bool {
    matches!(
        host.to_ascii_lowercase().as_str(),
        "metadata.google.internal" | "metadata" | "instance-data" | "instance-data.ec2.internal"
    )
}

#[cfg(test)]
mod tests {
    use serde_json::Value;
    use url::Url;

    use super::*;
    use crate::{
        agent::tools::openapi::{mapping::OpenApiRequestPlan, types::RuntimeOpenApiTarget},
        config::agent::AgentToolEgressPolicyConfig,
    };

    fn target() -> RuntimeOpenApiTarget {
        RuntimeOpenApiTarget {
            base_url: "https://api.example.test".to_string(),
            canonical_host: "api.example.test".to_string(),
            allowed_scheme: "https".to_string(),
            allowed_port: 443,
            path_template: "/v1/tickets".to_string(),
            ..Default::default()
        }
    }

    fn plan(url: &str) -> OpenApiRequestPlan {
        OpenApiRequestPlan {
            method: "GET".to_string(),
            url: Url::parse(url).expect("test URL should parse"),
            headers: Vec::new(),
            body: Option::<Value>::None,
            request_bytes: 0,
        }
    }

    fn default_policy() -> AgentToolEgressPolicyConfig {
        AgentToolEgressPolicyConfig::default()
    }

    #[test]
    fn rejects_http_when_snapshot_requires_https() {
        let error = validate_openapi_egress(
            &target(),
            &plan("http://api.example.test/v1"),
            &default_policy(),
        )
        .expect_err("http should not pass a https snapshot");

        assert_eq!(error, OpenApiEgressError::SchemeMismatch);
    }

    #[test]
    fn rejects_userinfo() {
        let error = validate_openapi_egress(
            &target(),
            &plan("https://user:pass@api.example.test/v1"),
            &default_policy(),
        )
        .expect_err("userinfo should be rejected");

        assert_eq!(error, OpenApiEgressError::UserInfoForbidden);
    }

    #[test]
    fn rejects_fragment() {
        let error = validate_openapi_egress(
            &target(),
            &plan("https://api.example.test/v1#fragment"),
            &default_policy(),
        )
        .expect_err("fragment should be rejected");

        assert_eq!(error, OpenApiEgressError::FragmentForbidden);
    }

    #[test]
    fn rejects_host_mismatch() {
        let error = validate_openapi_egress(
            &target(),
            &plan("https://evil.example.test/v1"),
            &default_policy(),
        )
        .expect_err("host mismatch should be rejected");

        assert_eq!(error, OpenApiEgressError::HostMismatch);
    }

    #[test]
    fn rejects_port_mismatch() {
        let error = validate_openapi_egress(
            &target(),
            &plan("https://api.example.test:8443/v1"),
            &default_policy(),
        )
        .expect_err("port mismatch should be rejected");

        assert_eq!(error, OpenApiEgressError::PortMismatch);
    }

    #[test]
    fn rejects_blocked_literal_hosts_for_p0_without_dns_resolution() {
        for (url, expected) in [
            (
                "https://localhost/v1",
                OpenApiEgressError::EgressPolicyBlocked,
            ),
            (
                "https://127.0.0.1/v1",
                OpenApiEgressError::EgressPolicyBlocked,
            ),
            (
                "https://10.0.0.1/v1",
                OpenApiEgressError::EgressPolicyBlocked,
            ),
            (
                "https://169.254.1.2/v1",
                OpenApiEgressError::EgressPolicyBlocked,
            ),
            (
                "https://169.254.169.254/latest",
                OpenApiEgressError::EgressPolicyBlocked,
            ),
        ] {
            let mut target = target();
            target.canonical_host = Url::parse(url)
                .expect("test URL should parse")
                .host_str()
                .expect("test URL should have host")
                .to_string();

            let error = validate_openapi_egress(&target, &plan(url), &default_policy())
                .expect_err("blocked literal host should be rejected");

            assert_eq!(error, expected);
        }
    }

    #[test]
    fn rejects_known_metadata_hostname_without_dns_resolution() {
        let mut target = target();
        target.canonical_host = "metadata.google.internal".to_string();

        let error = validate_openapi_egress(
            &target,
            &plan("https://metadata.google.internal/computeMetadata/v1"),
            &default_policy(),
        )
        .expect_err("known metadata hostname should be rejected");

        assert_eq!(error, OpenApiEgressError::MetadataHostBlocked);
    }

    #[test]
    fn accepts_final_url_when_snapshot_host_and_default_port_are_preserved() {
        validate_openapi_egress(
            &target(),
            &plan("https://api.example.test/v1/tickets?status=open"),
            &default_policy(),
        )
        .expect("snapshot host and default https port should pass");
    }

    #[test]
    fn accepts_final_url_when_snapshot_explicit_port_is_preserved() {
        let mut target = target();
        target.base_url = "https://api.example.test:8443".to_string();
        target.allowed_port = 8443;

        validate_openapi_egress(
            &target,
            &plan("https://api.example.test:8443/v1/tickets"),
            &default_policy(),
        )
        .expect("snapshot host and explicit port should pass");
    }
}
