use std::time::Duration;

use crate::types::provider::InferenceProvider;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct BusinessErrorCheck {
    pub provider: InferenceProvider,
    pub path: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Hash)]
pub struct DispatcherConfig {
    #[serde(default = "default_timeout", with = "humantime_serde")]
    pub timeout: Duration,
    #[serde(default = "default_connection_timeout", with = "humantime_serde")]
    pub connection_timeout: Duration,
    #[serde(
        default,
        rename = "business-error-checks",
        alias = "business_error_checks"
    )]
    pub business_error_checks: Vec<BusinessErrorCheck>,
}

impl DispatcherConfig {
    pub fn checks_business_errors(&self, provider: &InferenceProvider, path: &str) -> bool {
        let path = path
            .split('?')
            .next()
            .unwrap_or(path)
            .trim_start_matches('/');
        self.business_error_checks
            .iter()
            .any(|check| &check.provider == provider && check.path.trim_start_matches('/') == path)
    }
}

impl Default for DispatcherConfig {
    fn default() -> Self {
        Self {
            timeout: default_timeout(),
            connection_timeout: default_connection_timeout(),
            business_error_checks: Vec::new(),
        }
    }
}

#[cfg(feature = "testing")]
impl crate::tests::TestDefault for DispatcherConfig {
    fn test_default() -> Self {
        Self::default()
    }
}

fn default_timeout() -> Duration {
    Duration::from_secs(60 * 15)
}

fn default_connection_timeout() -> Duration {
    Duration::from_secs(10)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn business_error_policy_is_loaded_from_public_yaml_and_scoped_to_provider_and_endpoint() {
        let config: DispatcherConfig = serde_yml::from_str(
            "business-error-checks:\n  - provider: openai\n    path: v1/chat/completions\n",
        )
        .unwrap();
        assert!(
            config
                .checks_business_errors(&InferenceProvider::OpenAI, "/v1/chat/completions?trace=1")
        );
        assert!(
            !config.checks_business_errors(&InferenceProvider::Anthropic, "/v1/chat/completions")
        );
        assert!(
            !config.checks_business_errors(&InferenceProvider::OpenAI, "/v1/images/generations")
        );
        assert!(
            !DispatcherConfig::default()
                .checks_business_errors(&InferenceProvider::OpenAI, "/v1/chat/completions")
        );
    }
}
