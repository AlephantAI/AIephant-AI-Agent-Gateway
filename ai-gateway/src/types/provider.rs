use std::{str::FromStr, sync::Arc};

use compact_str::CompactString;
use rustc_hash::FxHashMap as HashMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use strum::{EnumIter, IntoEnumIterator};
use tokio::sync::RwLock;

use super::secret::Secret;
use crate::{
    config::Config, endpoints::ApiEndpoint, error::provider::ProviderError, metrics::Metrics,
    types::org::OrgId,
};

#[derive(
    Debug, Clone, Default, Copy, Eq, Hash, PartialEq, EnumIter, strum::Display, strum::EnumString,
)]
#[strum(serialize_all = "kebab-case")]
pub enum ModelProvider {
    #[default]
    OpenAI,
    Anthropic,
    Amazon,
    Deepseek,
    Google,
}

impl<'de> Deserialize<'de> for ModelProvider {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        ModelProvider::from_str(&s).map_err(serde::de::Error::custom)
    }
}

impl Serialize for ModelProvider {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

#[derive(
    Debug, Clone, Default, Eq, Hash, PartialEq, EnumIter, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum InferenceProvider {
    #[default]
    #[serde(rename = "openai")]
    OpenAI,
    Anthropic,
    Bedrock,
    Ollama,
    #[serde(rename = "gemini")]
    GoogleGemini,
    #[serde(rename = "custom")]
    Custom,
    #[serde(untagged)]
    Named(CompactString),
}

impl InferenceProvider {
    #[must_use]
    pub fn endpoints(&self) -> Vec<ApiEndpoint> {
        match self {
            InferenceProvider::OpenAI => crate::endpoints::openai::OpenAI::iter()
                .map(ApiEndpoint::OpenAI)
                .collect(),
            InferenceProvider::Anthropic => crate::endpoints::anthropic::Anthropic::iter()
                .map(ApiEndpoint::Anthropic)
                .collect(),
            InferenceProvider::Ollama => crate::endpoints::ollama::Ollama::iter()
                .map(ApiEndpoint::Ollama)
                .collect(),
            InferenceProvider::Bedrock => crate::endpoints::bedrock::Bedrock::iter()
                .map(ApiEndpoint::Bedrock)
                .collect(),
            InferenceProvider::GoogleGemini => crate::endpoints::google::Google::iter()
                .map(ApiEndpoint::Google)
                .collect(),
            InferenceProvider::Custom => crate::endpoints::openai::OpenAI::iter()
                .map(|endpoint| ApiEndpoint::OpenAICompatible {
                    provider: self.clone(),
                    openai_endpoint: endpoint,
                })
                .collect(),
            InferenceProvider::Named(_) => crate::endpoints::openai::OpenAI::iter()
                .map(|endpoint| ApiEndpoint::OpenAICompatible {
                    provider: self.clone(),
                    openai_endpoint: endpoint,
                })
                .collect(),
        }
    }

    /// Returns the `providers.code` value used in the DB for this provider.
    ///
    /// Used when querying `master_keys` by provider (e.g. workspace fallback).
    /// This can differ from `AsRef<str>` for historical compatibility (e.g.
    /// `GoogleGemini` is represented as `google` in DB provider codes).
    #[must_use]
    pub fn as_provider_code(&self) -> &str {
        match self {
            InferenceProvider::GoogleGemini => "google",
            _ => self.as_ref(),
        }
    }

    pub fn from_provider_code(provider_name: &str) -> Result<Self, ProviderError> {
        let normalized = provider_name.trim().to_ascii_lowercase();
        let provider = match normalized.as_str() {
            "openai" => InferenceProvider::OpenAI,
            "anthropic" => InferenceProvider::Anthropic,
            "aws bedrock" | "bedrock" | "amazon" => InferenceProvider::Bedrock,
            "ollama" => InferenceProvider::Ollama,
            "google" | "google ai (gemini)" | "gemini" => InferenceProvider::GoogleGemini,
            "custom" => InferenceProvider::Custom,

            // Legacy aliases.
            "mistral ai" => InferenceProvider::Named("mistral".into()),
            "zai" => InferenceProvider::Named("z-ai".into()),
            "x.ai (grok)" | "xai" => InferenceProvider::Named("x-ai".into()),

            _ if is_valid_named_provider_code(&normalized) => {
                InferenceProvider::Named(normalized.into())
            }
            _ => {
                return Err(ProviderError::InvalidProviderName(provider_name.into()));
            }
        };
        Ok(provider)
    }

    /// Static validation only (`Config::validate_model_mappings`): OpenRouter
    /// is treated as an aggregator so routing YAML does not need to
    /// enumerate every upstream model ID.
    ///
    /// Runtime skip uses
    /// [`crate::app_state::AppState::provider_skips_model_mapping_catalog`].
    #[must_use]
    pub fn skips_model_mapping_catalog_for_static_validation(&self) -> bool {
        matches!(
            self,
            InferenceProvider::Named(name) if name.as_str() == "openrouter"
        )
    }
}

fn is_valid_named_provider_code(code: &str) -> bool {
    !code.is_empty()
        && !code.starts_with('-')
        && !code.ends_with('-')
        && code
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

impl FromStr for InferenceProvider {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> ::core::result::Result<InferenceProvider, Self::Err> {
        let normalized = s.trim();
        Ok(InferenceProvider::from_provider_code(normalized)
            .unwrap_or_else(|_| InferenceProvider::Named(normalized.into())))
    }
}

impl AsRef<str> for InferenceProvider {
    fn as_ref(&self) -> &str {
        match self {
            InferenceProvider::Named(name) => name.as_ref(),
            InferenceProvider::OpenAI => "openai",
            InferenceProvider::Anthropic => "anthropic",
            InferenceProvider::Bedrock => "bedrock",
            InferenceProvider::Ollama => "ollama",
            InferenceProvider::GoogleGemini => "gemini",
            InferenceProvider::Custom => "custom",
        }
    }
}

impl std::fmt::Display for InferenceProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InferenceProvider::Named(name) => write!(f, "{name}"),
            _ => write!(f, "{}", self.as_ref()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ProviderKey {
    Secret(Secret<String>),
    AwsCredentials {
        access_key: Secret<String>,
        secret_key: Secret<String>,
    },
    NotRequired,
}

impl ProviderKey {
    #[must_use]
    pub fn as_secret(&self) -> Option<&Secret<String>> {
        match self {
            ProviderKey::Secret(key) => Some(key),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_aws_credentials(&self) -> (Option<&Secret<String>>, Option<&Secret<String>>) {
        match self {
            ProviderKey::AwsCredentials {
                access_key,
                secret_key,
            } => (Some(access_key), Some(secret_key)),
            _ => (None, None),
        }
    }
}

#[derive(Debug)]
pub struct ProviderKeys(RwLock<HashMap<OrgId, ProviderKeyMap>>);

impl ProviderKeys {
    #[must_use]
    pub fn new(_config: &Config, _metrics: &Metrics) -> Self {
        Self(RwLock::new(HashMap::default()))
    }

    pub async fn set_all_provider_keys(&self, provider_keys: HashMap<OrgId, ProviderKeyMap>) {
        let mut keys = self.0.write().await;
        *keys = provider_keys;
    }

    pub async fn set_org_provider_keys(&self, org_id: OrgId, provider_keys: ProviderKeyMap) {
        let mut keys = self.0.write().await;
        keys.insert(org_id, provider_keys);
    }

    pub async fn get_provider_key(
        &self,
        provider: &InferenceProvider,
        org_id: Option<&OrgId>,
    ) -> Option<ProviderKey> {
        let org_id = org_id?;
        let keys = self.0.read().await;
        keys.get(org_id)
            .and_then(|org_keys| org_keys.get(provider))
            .cloned()
    }
}

#[derive(Debug, Clone)]
pub struct ProviderKeyMap(Arc<HashMap<InferenceProvider, ProviderKey>>);

impl std::ops::Deref for ProviderKeyMap {
    type Target = HashMap<InferenceProvider, ProviderKey>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl ProviderKeyMap {
    #[must_use]
    pub fn from_db(provider_keys: HashMap<InferenceProvider, ProviderKey>) -> Self {
        Self(Arc::new(provider_keys))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inference_provider_as_ref() {
        let named_provider = InferenceProvider::Named("test".into());
        let named_provider_str = named_provider.as_ref();
        assert_eq!("test", named_provider_str);
    }

    #[test]
    fn inference_provider_to_string() {
        let named_provider = InferenceProvider::Named("test".into());
        let named_provider_str = named_provider.to_string();
        assert_eq!("test", named_provider_str);
    }

    #[test]
    fn inference_provider_as_provider_code_uses_db_code_mapping() {
        assert_eq!(InferenceProvider::OpenAI.as_provider_code(), "openai");
        assert_eq!(InferenceProvider::Anthropic.as_provider_code(), "anthropic");
        assert_eq!(InferenceProvider::Bedrock.as_provider_code(), "bedrock");
        assert_eq!(InferenceProvider::GoogleGemini.as_provider_code(), "google");
        assert_eq!(
            InferenceProvider::Named("mistral".into()).as_provider_code(),
            "mistral"
        );
    }

    #[test]
    fn from_provider_code_accepts_db_and_legacy_codes() {
        assert_eq!(
            InferenceProvider::from_provider_code("openai").unwrap(),
            InferenceProvider::OpenAI
        );
        assert_eq!(
            InferenceProvider::from_provider_code("Anthropic").unwrap(),
            InferenceProvider::Anthropic
        );
        assert_eq!(
            InferenceProvider::from_provider_code("google").unwrap(),
            InferenceProvider::GoogleGemini
        );
        assert_eq!(
            InferenceProvider::from_provider_code("Google AI (Gemini)").unwrap(),
            InferenceProvider::GoogleGemini
        );
        assert_eq!(
            InferenceProvider::from_provider_code("gemini").unwrap(),
            InferenceProvider::GoogleGemini
        );
        assert_eq!(
            InferenceProvider::from_provider_code("amazon").unwrap(),
            InferenceProvider::Bedrock
        );
    }

    #[test]
    fn from_str_accepts_google_db_code_for_gemini() {
        assert_eq!(
            InferenceProvider::from_str("google").unwrap(),
            InferenceProvider::GoogleGemini
        );
    }

    #[test]
    fn from_provider_code_accepts_qwen_minimax_and_moonshotai() {
        assert_eq!(
            InferenceProvider::from_provider_code("qwen").unwrap(),
            InferenceProvider::Named("qwen".into())
        );
        assert_eq!(
            InferenceProvider::from_provider_code("minimax").unwrap(),
            InferenceProvider::Named("minimax".into())
        );
        assert_eq!(
            InferenceProvider::from_provider_code("minimax-cn").unwrap(),
            InferenceProvider::Named("minimax-cn".into())
        );
        assert_eq!(
            InferenceProvider::from_provider_code("moonshotai").unwrap(),
            InferenceProvider::Named("moonshotai".into())
        );
    }

    #[test]
    fn from_provider_code_accepts_openrouter() {
        assert_eq!(
            InferenceProvider::from_provider_code("openrouter").unwrap(),
            InferenceProvider::Named("openrouter".into())
        );
        assert_eq!(
            InferenceProvider::from_provider_code("OpenRouter").unwrap(),
            InferenceProvider::Named("openrouter".into())
        );
    }

    #[test]
    fn from_provider_code_accepts_z_ai() {
        assert_eq!(
            InferenceProvider::from_provider_code("z-ai").unwrap(),
            InferenceProvider::Named("z-ai".into())
        );
        assert_eq!(
            InferenceProvider::from_provider_code("Z-AI").unwrap(),
            InferenceProvider::Named("z-ai".into())
        );
        assert_eq!(
            InferenceProvider::from_provider_code("zai").unwrap(),
            InferenceProvider::Named("z-ai".into())
        );
    }

    #[test]
    fn from_provider_code_accepts_x_ai() {
        assert_eq!(
            InferenceProvider::from_provider_code("x-ai").unwrap(),
            InferenceProvider::Named("x-ai".into())
        );
        assert_eq!(
            InferenceProvider::from_provider_code("x.ai (grok)").unwrap(),
            InferenceProvider::Named("x-ai".into())
        );
        assert_eq!(
            InferenceProvider::from_provider_code("xai").unwrap(),
            InferenceProvider::Named("x-ai".into())
        );
    }

    #[test]
    fn from_provider_code_covers_all_db_seed_provider_codes() {
        let provider_codes = [
            "ai21",
            "aion-labs",
            "alfredpros",
            "alibaba",
            "allenai",
            "alpindale",
            "amazon",
            "anthracite-org",
            "anthropic",
            "arcee-ai",
            "baidu",
            "bytedance",
            "bytedance-seed",
            "cognitivecomputations",
            "cohere",
            "deepcogito",
            "deepseek",
            "eleutherai",
            "essentialai",
            "google",
            "gryphe",
            "ibm-granite",
            "inception",
            "inflection",
            "kwaipilot",
            "liquid",
            "mancer",
            "meituan",
            "meta-llama",
            "microsoft",
            "minimax",
            "mistral",
            "mistralai",
            "moonshotai",
            "morph",
            "nex-agi",
            "nousresearch",
            "nvidia",
            "openai",
            "openrouter",
            "perceptron",
            "perplexity",
            "prime-intellect",
            "qwen",
            "qwen-beijing",
            "qwen-us",
            "reka",
            "rekaai",
            "relace",
            "sao10k",
            "stepfun",
            "switchpoint",
            "tencent",
            "thedrummer",
            "tngtech",
            "undi95",
            "upstage",
            "writer",
            "x-ai",
            "xiaomi",
            "z-ai",
            "z-ai-cn",
            "z-ai-coding-cn",
        ];

        for code in provider_codes {
            assert!(
                InferenceProvider::from_provider_code(code).is_ok(),
                "provider code should be accepted: {code}"
            );
        }
    }

    #[test]
    fn from_provider_code_accepts_valid_dynamic_named_provider_codes() {
        assert_eq!(
            InferenceProvider::from_provider_code("new-provider-42").unwrap(),
            InferenceProvider::Named("new-provider-42".into())
        );
        assert_eq!(
            InferenceProvider::from_provider_code("QWEN-BEIJING").unwrap(),
            InferenceProvider::Named("qwen-beijing".into())
        );
    }

    #[test]
    fn from_provider_code_rejects_invalid_dynamic_named_provider_codes() {
        for code in ["", " ", "-qwen", "qwen-", "qwen/us", "qwen us"] {
            assert!(
                InferenceProvider::from_provider_code(code).is_err(),
                "provider code should be rejected: {code:?}"
            );
        }
    }

    #[test]
    fn from_provider_code_custom() {
        let provider = InferenceProvider::from_provider_code("custom").unwrap();
        assert_eq!(provider, InferenceProvider::Custom);
    }

    #[test]
    fn from_provider_code_custom_case_insensitive() {
        let provider = InferenceProvider::from_provider_code("Custom").unwrap();
        assert_eq!(provider, InferenceProvider::Custom);
    }

    #[test]
    fn custom_as_ref_returns_custom() {
        assert_eq!(InferenceProvider::Custom.as_ref(), "custom");
    }

    #[test]
    fn custom_as_provider_code_returns_custom() {
        assert_eq!(InferenceProvider::Custom.as_provider_code(), "custom");
    }
}
