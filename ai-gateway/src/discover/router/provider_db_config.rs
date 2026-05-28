//! Converts raw DB rows from `providers` / `provider_models` into the
//! in-memory structures used by the gateway:
//!
//! * [`ProvidersConfig`] — global provider registry (base URL + models).
//! * [`BareModelExpandIndex`] — bare `model_id` expansion for DB models.
//!
//! The conversion uses embedded provider metadata for URL/auth/version fallback
//! only. DB mode owns the model catalog: models come from `provider_models`
//! and this path must not parse YAML models.

use std::collections::HashMap;

use indexmap::IndexSet;
use tracing::{debug, warn};
use uuid::Uuid;

use super::bare_model_expand_index::BareModelExpandIndex;
use crate::{
    config::providers::{
        EmbeddedProviderMetadata, GlobalProviderConfig, ProvidersConfig,
    },
    store::router::{DbGatewayProvider, DbGatewayProviderModel},
    types::{model_id::ModelId, provider::InferenceProvider},
};

/// Build [`ProvidersConfig`] and bare model expansion from raw DB rows.
///
/// Returns:
/// * `ProvidersConfig` — updated view of all enabled providers.
/// * [`BareModelExpandIndex`] — bare `model_id` → `code/model` (aligned with DB
///   rows successfully ingested into `GlobalProviderConfig.models`).
#[allow(clippy::too_many_lines)]
pub fn build_from_db(
    db_providers: &[DbGatewayProvider],
    db_models: &[DbGatewayProviderModel],
) -> (ProvidersConfig, BareModelExpandIndex) {
    // Index raw model name strings by provider_id for O(1) lookup.
    let mut raw_models_by_provider: HashMap<Uuid, Vec<String>> = HashMap::new();
    for row in db_models {
        raw_models_by_provider
            .entry(row.provider_id)
            .or_default()
            .push(row.model_id.clone());
    }

    // DB mode owns the model catalog; this fallback must not parse YAML models.
    let embedded_defaults = EmbeddedProviderMetadata::cached();

    let mut entries: Vec<(InferenceProvider, GlobalProviderConfig)> =
        Vec::new();
    let mut bare_model_expand = BareModelExpandIndex::default();

    for db_provider in db_providers {
        let Ok(provider) =
            InferenceProvider::from_provider_code(&db_provider.code)
        else {
            warn!(
                code = %db_provider.code,
                "provider_db_config: unknown provider code, skipping"
            );
            continue;
        };

        // Base URL: DB override takes precedence; fall back to embedded YAML.
        let base_url = if let Some(url_str) = &db_provider.default_base_url {
            match url_str.parse::<url::Url>() {
                Ok(u) => u,
                Err(e) => {
                    warn!(
                        code = %db_provider.code,
                        url = %url_str,
                        error = %e,
                        "provider_db_config: invalid base_url, falling back to embedded default"
                    );
                    if let Some(c) = embedded_defaults.get(&provider) {
                        c.base_url.clone()
                    } else {
                        warn!(
                            code = %db_provider.code,
                            "provider_db_config: no base_url and no embedded default, skipping provider"
                        );
                        continue;
                    }
                }
            }
        } else if let Some(c) = embedded_defaults.get(&provider) {
            c.base_url.clone()
        } else {
            if !matches!(provider, InferenceProvider::Custom) {
                warn!(
                    code = %db_provider.code,
                    "provider_db_config: no base_url in DB and no embedded default, skipping provider"
                );
            }
            continue;
        };

        let cn_base_url = if let Some(url_str) = &db_provider.cn_base_url {
            match url_str.parse::<url::Url>() {
                Ok(u) => Some(u),
                Err(e) => {
                    warn!(
                        code = %db_provider.code,
                        url = %url_str,
                        error = %e,
                        "provider_db_config: invalid cn_base_url, ignoring"
                    );
                    None
                }
            }
        } else {
            embedded_defaults
                .get(&provider)
                .and_then(|c| c.cn_base_url.clone())
        };

        // Convert raw model name strings to typed ModelId using provider
        // context.
        let raw_models = raw_models_by_provider
            .remove(&db_provider.id)
            .unwrap_or_default();
        let mut models = IndexSet::new();
        for model_str in &raw_models {
            match ModelId::from_str_and_provider(provider.clone(), model_str) {
                Ok(m) => {
                    models.insert(m);
                    bare_model_expand.push(&db_provider.code, model_str);
                }
                Err(e) => {
                    warn!(
                        code = %db_provider.code,
                        model = %model_str,
                        error = %e,
                        "provider_db_config: failed to parse model_id, skipping"
                    );
                }
            }
        }

        // Preserve version header from embedded config (e.g. Anthropic).
        let version = embedded_defaults
            .get(&provider)
            .and_then(|c| c.version.clone());

        // Preserve upstream auth style from embedded config (e.g. Xiaomi
        // `api-key`).
        let upstream_auth = embedded_defaults
            .get(&provider)
            .map(|c| c.upstream_auth)
            .unwrap_or_default();

        debug!(
            code = %db_provider.code,
            models = models.len(),
            "provider_db_config: registered provider"
        );

        entries.push((
            provider.clone(),
            GlobalProviderConfig {
                models,
                base_url,
                cn_base_url,
                version,
                upstream_auth,
            },
        ));
    }

    let providers_config: ProvidersConfig = entries.into_iter().collect();
    (providers_config, bare_model_expand)
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use indexmap::IndexSet;
    use uuid::Uuid;

    use super::{super::BareModelExpandIndex, *};

    #[test]
    fn build_from_db_uses_db_base_url_without_building_router() {
        let provider_id = Uuid::new_v4();
        let db_providers = vec![DbGatewayProvider {
            id: provider_id,
            code: "openai".to_string(),
            default_base_url: Some("https://override.openai.test".to_string()),
            cn_base_url: None,
            updated_at: Utc::now(),
            is_router: false,
        }];
        let db_models = vec![
            DbGatewayProviderModel {
                provider_id,
                model_id: "gpt-4o".to_string(),
            },
            DbGatewayProviderModel {
                provider_id,
                model_id: String::new(), // invalid model id should be skipped
            },
        ];

        let (providers_config, bare_expand) =
            build_from_db(&db_providers, &db_models);

        let openai_cfg = providers_config
            .get(&InferenceProvider::OpenAI)
            .expect("openai config should exist");
        assert_eq!(
            openai_cfg.base_url.as_str(),
            "https://override.openai.test/"
        );

        let expected_models: IndexSet<ModelId> =
            IndexSet::from_iter([ModelId::from_str_and_provider(
                InferenceProvider::OpenAI,
                "gpt-4o",
            )
            .expect("valid model")]);
        assert_eq!(openai_cfg.models, expected_models);

        assert_eq!(
            bare_expand.gateway_models_for_bare_id("gpt-4o"),
            vec!["openai/gpt-4o".to_string()]
        );
    }

    #[test]
    fn build_from_db_uses_db_cn_base_url_when_present() {
        let provider_id = Uuid::new_v4();
        let db_providers = vec![DbGatewayProvider {
            id: provider_id,
            code: "minimax".to_string(),
            default_base_url: Some("https://api.minimax.io/".to_string()),
            cn_base_url: Some("https://api.minimaxi.com/".to_string()),
            updated_at: Utc::now(),
            is_router: false,
        }];
        let db_models = vec![DbGatewayProviderModel {
            provider_id,
            model_id: "minimax-m1".to_string(),
        }];

        let (providers_config, _) = build_from_db(&db_providers, &db_models);
        let cfg = providers_config
            .get(&InferenceProvider::Named("minimax".into()))
            .expect("minimax config should exist");
        assert_eq!(
            cfg.cn_base_url.as_ref().map(url::Url::as_str),
            Some("https://api.minimaxi.com/")
        );
    }

    #[test]
    fn build_from_db_ignores_invalid_db_cn_base_url() {
        let provider_id = Uuid::new_v4();
        let db_providers = vec![DbGatewayProvider {
            id: provider_id,
            code: "minimax".to_string(),
            default_base_url: Some("https://api.minimax.io/".to_string()),
            cn_base_url: Some("not a url".to_string()),
            updated_at: Utc::now(),
            is_router: false,
        }];
        let db_models = vec![DbGatewayProviderModel {
            provider_id,
            model_id: "minimax-m1".to_string(),
        }];

        let (providers_config, _) = build_from_db(&db_providers, &db_models);
        let provider = InferenceProvider::Named("minimax".into());
        let cfg = providers_config
            .get(&provider)
            .expect("minimax config should still exist");
        assert_eq!(cfg.cn_base_url, None);
    }

    #[test]
    fn build_from_db_skips_unknown_provider_codes() {
        let db_providers = vec![DbGatewayProvider {
            id: Uuid::new_v4(),
            code: "Invalid Provider!".to_string(),
            default_base_url: Some("https://unknown.test".to_string()),
            cn_base_url: None,
            updated_at: Utc::now(),
            is_router: false,
        }];

        let (providers_config, bare_expand) = build_from_db(&db_providers, &[]);
        assert!(providers_config.is_empty());
        assert_eq!(bare_expand, BareModelExpandIndex::default());
    }

    #[test]
    fn build_from_db_registers_provider_absent_from_yaml_when_db_url_present() {
        let provider_id = Uuid::new_v4();
        let db_providers = vec![DbGatewayProvider {
            id: provider_id,
            code: "db-only-provider-42".to_string(),
            default_base_url: Some(
                "https://api.db-only-provider-42.test/v1/".to_string(),
            ),
            cn_base_url: None,
            updated_at: Utc::now(),
            is_router: false,
        }];
        let db_models = vec![DbGatewayProviderModel {
            provider_id,
            model_id: "db-only-model-a".to_string(),
        }];

        let (providers_config, bare_expand) =
            build_from_db(&db_providers, &db_models);

        let provider = InferenceProvider::Named("db-only-provider-42".into());
        let cfg = providers_config
            .get(&provider)
            .expect("DB-only provider config should exist");
        assert_eq!(
            cfg.base_url.as_str(),
            "https://api.db-only-provider-42.test/v1/"
        );
        assert_eq!(cfg.cn_base_url, None);
        assert_eq!(cfg.version, None);
        assert_eq!(cfg.upstream_auth, Default::default());

        let expected_models: IndexSet<ModelId> =
            IndexSet::from_iter([ModelId::from_str_and_provider(
                provider.clone(),
                "db-only-model-a",
            )
            .expect("valid model")]);
        assert_eq!(cfg.models, expected_models);

        let bare = bare_expand.gateway_models_for_bare_id("db-only-model-a");
        assert_eq!(
            bare,
            vec!["db-only-provider-42/db-only-model-a".to_string()]
        );
    }

    #[test]
    fn build_from_db_uses_metadata_fallback_without_yaml_models() {
        let provider_id = Uuid::new_v4();
        let db_providers = vec![DbGatewayProvider {
            id: provider_id,
            code: "anthropic".to_string(),
            default_base_url: None,
            cn_base_url: None,
            updated_at: Utc::now(),
            is_router: false,
        }];
        let db_models = vec![DbGatewayProviderModel {
            provider_id,
            model_id: "claude-sonnet-4-20250514".to_string(),
        }];

        let (providers_config, bare_expand) =
            build_from_db(&db_providers, &db_models);

        let cfg = providers_config
            .get(&InferenceProvider::Anthropic)
            .expect("anthropic config should exist");
        assert_eq!(cfg.base_url.as_str(), "https://api.anthropic.com/");
        assert_eq!(
            cfg.version.as_deref(),
            Some(crate::config::providers::DEFAULT_ANTHROPIC_VERSION)
        );

        let expected_models: IndexSet<ModelId> =
            IndexSet::from_iter([ModelId::from_str_and_provider(
                InferenceProvider::Anthropic,
                "claude-sonnet-4-20250514",
            )
            .expect("valid model")]);
        assert_eq!(cfg.models, expected_models);

        let bare =
            bare_expand.gateway_models_for_bare_id("claude-sonnet-4-20250514");
        assert_eq!(
            bare,
            vec!["anthropic/claude-sonnet-4-20250514".to_string()]
        );
    }

    #[test]
    fn build_from_db_skips_bedrock_short_model_ids_without_normalization() {
        let provider_id = Uuid::new_v4();
        let db_providers = vec![DbGatewayProvider {
            id: provider_id,
            code: "amazon".to_string(),
            default_base_url: Some("https://bedrock-runtime.test/".to_string()),
            cn_base_url: None,
            updated_at: Utc::now(),
            is_router: false,
        }];
        let db_models = vec![
            DbGatewayProviderModel {
                provider_id,
                model_id: "nova-pro-v1".to_string(),
            },
            DbGatewayProviderModel {
                provider_id,
                model_id: "amazon.nova-lite-v1:0".to_string(),
            },
        ];

        let (providers_config, bare_expand) =
            build_from_db(&db_providers, &db_models);

        let bedrock_cfg = providers_config
            .get(&InferenceProvider::Bedrock)
            .expect("amazon should map to bedrock config");
        assert_eq!(
            bedrock_cfg.base_url.as_str(),
            "https://bedrock-runtime.test/"
        );

        let expected_models: IndexSet<ModelId> =
            IndexSet::from_iter([ModelId::from_str_and_provider(
                InferenceProvider::Bedrock,
                "amazon.nova-lite-v1:0",
            )
            .expect("valid bedrock model")]);
        assert_eq!(bedrock_cfg.models, expected_models);

        assert!(
            bare_expand
                .gateway_models_for_bare_id("nova-pro-v1")
                .is_empty()
        );
        assert_eq!(
            bare_expand.gateway_models_for_bare_id("amazon.nova-lite-v1:0"),
            vec!["amazon/amazon.nova-lite-v1:0".to_string()]
        );
    }

    #[test]
    fn build_from_db_registers_z_ai_code() {
        let provider_id = Uuid::new_v4();
        let db_providers = vec![DbGatewayProvider {
            id: provider_id,
            code: "z-ai".to_string(),
            default_base_url: Some("https://api.z.ai/api/paas/v4/".to_string()),
            cn_base_url: None,
            updated_at: Utc::now(),
            is_router: false,
        }];
        let db_models = vec![DbGatewayProviderModel {
            provider_id,
            model_id: "glm-5".to_string(),
        }];

        let (providers_config, _bare_expand) =
            build_from_db(&db_providers, &db_models);

        let z_ai = InferenceProvider::Named("z-ai".into());
        let cfg = providers_config.get(&z_ai).expect("z-ai providers config");
        assert_eq!(cfg.base_url.as_str(), "https://api.z.ai/api/paas/v4/");
    }

    #[test]
    fn build_from_db_falls_back_to_embedded_base_url_when_db_url_missing() {
        let provider_id = Uuid::new_v4();
        let db_providers = vec![DbGatewayProvider {
            id: provider_id,
            code: "anthropic".to_string(),
            default_base_url: None,
            cn_base_url: None,
            updated_at: Utc::now(),
            is_router: false,
        }];

        let (providers_config, _bare_expand) =
            build_from_db(&db_providers, &[]);

        let embedded_defaults = EmbeddedProviderMetadata::cached();
        let expected_base_url = embedded_defaults
            .get(&InferenceProvider::Anthropic)
            .expect("embedded anthropic exists")
            .base_url
            .clone();
        let anthropic_cfg = providers_config
            .get(&InferenceProvider::Anthropic)
            .expect("anthropic config should exist");
        assert_eq!(anthropic_cfg.base_url, expected_base_url);
    }

    /// When the same `model_id` exists under two `providers.code` values, the
    /// expand index lists two `code/model` entries (`code` must parse via
    /// [`InferenceProvider::from_provider_code`]).
    #[test]
    fn build_from_db_bare_index_lists_all_providers_for_same_model_id() {
        let prov_groq = Uuid::new_v4();
        let prov_deepseek = Uuid::new_v4();
        let db_providers = vec![
            DbGatewayProvider {
                id: prov_groq,
                code: "groq".to_string(),
                default_base_url: Some("https://groq.test".to_string()),
                cn_base_url: None,
                updated_at: Utc::now(),
                is_router: false,
            },
            DbGatewayProvider {
                id: prov_deepseek,
                code: "deepseek".to_string(),
                default_base_url: Some("https://deepseek.test".to_string()),
                cn_base_url: None,
                updated_at: Utc::now(),
                is_router: false,
            },
        ];
        let shared = "gpt-4o";
        let db_models = vec![
            DbGatewayProviderModel {
                provider_id: prov_groq,
                model_id: shared.to_string(),
            },
            DbGatewayProviderModel {
                provider_id: prov_deepseek,
                model_id: shared.to_string(),
            },
        ];

        let (_cfg, bare) = build_from_db(&db_providers, &db_models);
        let v = bare.gateway_models_for_bare_id(shared);
        assert_eq!(v.len(), 2, "{v:?}");
        assert!(v.contains(&"groq/gpt-4o".to_string()));
        assert!(v.contains(&"deepseek/gpt-4o".to_string()));
    }
}
