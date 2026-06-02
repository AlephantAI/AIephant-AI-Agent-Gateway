#![allow(dead_code)]

use std::{
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use serde::Deserialize;

use crate::{
    app_state::AppState,
    middleware::model_support::catalog_redis_key,
    types::{model_id::ModelId, provider::InferenceProvider, usage_tokens::UsageTokenCounts},
};

/// Catalog pricing from Redis / DB `info` JSON.
/// `prompt`, `completion`, and `input_cache_read` are **USD per token** (not
/// per million tokens).
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub(crate) struct ModelInfo {
    #[serde(default)]
    pub(crate) schema_version: u32,
    pub(crate) prompt: f64,
    pub(crate) completion: f64,
    #[serde(default)]
    pub(crate) input_cache_read: Option<f64>,
    #[serde(default)]
    pub(crate) tag: Option<String>,
    #[serde(default)]
    pub(crate) create_time: Option<i64>,
    #[serde(default)]
    pub(crate) max_context_tokens: Option<u32>,
    #[serde(default)]
    pub(crate) max_completion_tokens: Option<u32>,
    #[serde(default)]
    pub(crate) model_interaction_type: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct CostBreakdown {
    pub(crate) prompt_cost: f64,
    pub(crate) completion_cost: f64,
    pub(crate) input_cache_read_cost: f64,
    pub(crate) total_cost: f64,
}

fn candidate_provider_codes(provider: &InferenceProvider) -> Vec<&str> {
    let request_side = provider.as_ref();
    let canonical = provider.as_provider_code();
    if request_side.eq_ignore_ascii_case(canonical) {
        vec![request_side]
    } else {
        vec![request_side, canonical]
    }
}

pub(crate) fn candidate_redis_keys(provider: &InferenceProvider, model: &ModelId) -> Vec<String> {
    let model_name = model.as_model_name().to_string();
    candidate_provider_codes(provider)
        .into_iter()
        .map(|provider_code| catalog_redis_key(provider_code, &model_name))
        .collect()
}

pub(crate) fn parse_model_info_str(raw: &str) -> Option<ModelInfo> {
    serde_json::from_str(raw).ok()
}

pub(crate) fn parse_model_info_value(value: serde_json::Value) -> Option<ModelInfo> {
    serde_json::from_value(value).ok()
}

/// Policy-only: estimated input cost in **micro-USD** for pre-flight Evaluate.
/// `info.prompt` is USD per token; micro-USD = `tokens * prompt * 1e6`.
#[must_use]
pub(crate) fn policy_estimated_input_micro_usd(
    estimated_input_tokens: u32,
    info: &ModelInfo,
) -> f64 {
    f64::from(estimated_input_tokens) * info.prompt * 1_000_000.0
}

pub(crate) fn calculate_cost(info: &ModelInfo, usage: &UsageTokenCounts) -> CostBreakdown {
    let prompt_cost = usage.prompt_tokens as f64 * info.prompt;
    let completion_cost = usage.completion_tokens as f64 * info.completion;
    let input_cache_read_cost =
        usage.prompt_cache_read_tokens as f64 * info.input_cache_read.unwrap_or(0.0);
    let total_cost = prompt_cost + completion_cost + input_cache_read_cost;

    CostBreakdown {
        prompt_cost,
        completion_cost,
        input_cache_read_cost,
        total_cost,
    }
}

pub(crate) async fn lookup_model_info_with_loaders<FR, FutR, FD, FutD>(
    provider: &InferenceProvider,
    model: &ModelId,
    mut load_redis: FR,
    mut load_db: FD,
) -> Option<ModelInfo>
where
    FR: FnMut(String) -> FutR,
    FutR: Future<Output = Option<String>>,
    FD: FnMut(String, String) -> FutD,
    FutD: Future<Output = Option<serde_json::Value>>,
{
    for key in candidate_redis_keys(provider, model) {
        if let Some(raw) = load_redis(key).await
            && let Some(info) = parse_model_info_str(&raw)
        {
            return Some(info);
        }
    }

    let model_name = model.as_model_name().to_string();
    for provider_code in candidate_provider_codes(provider) {
        if let Some(value) = load_db(provider_code.to_string(), model_name.clone()).await
            && let Some(info) = parse_model_info_value(value)
        {
            return Some(info);
        }
    }

    None
}

pub(crate) async fn lookup_model_info(
    app_state: &AppState,
    provider: &InferenceProvider,
    model: &ModelId,
) -> Option<ModelInfo> {
    let redis_failed = Arc::new(AtomicBool::new(false));

    lookup_model_info_with_loaders(
        provider,
        model,
        {
            let redis_failed = Arc::clone(&redis_failed);
            let app_state = app_state.clone();
            move |key| {
                let app_state = app_state.clone();
                let redis_failed = Arc::clone(&redis_failed);
                async move {
                    if redis_failed.load(Ordering::Relaxed) {
                        return None;
                    }
                    let Some(redis) = app_state.redis() else {
                        return None;
                    };
                    match redis.get_string(&key).await {
                        Ok(value) => value,
                        Err(error) => {
                            redis_failed.store(true, Ordering::Relaxed);
                            tracing::warn!(
                                error = %error,
                                %key,
                                "logger: model info redis lookup failed"
                            );
                            None
                        }
                    }
                }
            }
        },
        {
            let app_state = app_state.clone();
            move |provider_code, model_id| {
                let app_state = app_state.clone();
                async move {
                    let Some(store) = app_state.router_store() else {
                        return None;
                    };
                    match store
                        .get_model_info_for_gateway_model(&provider_code, &model_id)
                        .await
                    {
                        Ok(value) => value,
                        Err(error) => {
                            tracing::warn!(
                                error = %error,
                                %provider_code,
                                %model_id,
                                "logger: model info db lookup failed"
                            );
                            None
                        }
                    }
                }
            }
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use crate::types::{
        model_id::ModelId, provider::InferenceProvider, usage_tokens::UsageTokenCounts,
    };

    #[test]
    fn candidate_redis_keys_try_request_alias_before_canonical_provider() {
        let model = ModelId::Unknown("gemini-1.5-flash".to_string());

        let keys = super::candidate_redis_keys(&InferenceProvider::GoogleGemini, &model);

        assert_eq!(
            keys,
            vec![
                "gemini::gemini-1.5-flash".to_string(),
                "google::gemini-1.5-flash".to_string(),
            ]
        );
    }

    #[test]
    fn candidate_redis_keys_dedup_when_alias_matches_canonical() {
        let model = ModelId::Unknown("gpt-4o".to_string());

        let keys = super::candidate_redis_keys(&InferenceProvider::OpenAI, &model);

        assert_eq!(keys, vec!["openai::gpt-4o".to_string()]);
    }

    #[test]
    fn calculate_cost_uses_prompt_completion_and_cache_read_rates() {
        // USD per token ($3 / 1M prompt tok, $12 / 1M completion, etc.).
        let info = super::ModelInfo {
            schema_version: 1,
            prompt: 3e-6,
            completion: 12e-6,
            input_cache_read: Some(1.5e-6),
            tag: None,
            create_time: None,
            max_context_tokens: None,
            max_completion_tokens: None,
            model_interaction_type: None,
        };
        let usage = UsageTokenCounts {
            prompt_tokens: 1_000,
            completion_tokens: 500,
            prompt_cache_read_tokens: 250,
            ..UsageTokenCounts::default()
        };

        let got = super::calculate_cost(&info, &usage);

        assert!((got.prompt_cost - 0.003).abs() < 1e-12);
        assert!((got.completion_cost - 0.006).abs() < 1e-12);
        assert!((got.input_cache_read_cost - 0.000375).abs() < 1e-12);
        assert!((got.total_cost - 0.009375).abs() < 1e-12);
    }

    #[test]
    fn policy_estimated_input_micro_usd_matches_prompt_scale() {
        let info = super::ModelInfo {
            schema_version: 1,
            prompt: 3e-6,
            completion: 0.0,
            input_cache_read: None,
            tag: None,
            create_time: None,
            max_context_tokens: None,
            max_completion_tokens: None,
            model_interaction_type: None,
        };
        let got = super::policy_estimated_input_micro_usd(1_000, &info);
        assert!((got - 3000.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn lookup_model_info_falls_back_to_db_after_redis_miss() {
        let provider = InferenceProvider::Named("deepseek".into());
        let model = ModelId::Unknown("deepseek-chat".into());

        let got = super::lookup_model_info_with_loaders(
            &provider,
            &model,
            |_key| async { None },
            |_provider_code, _model_id| async {
                Some(serde_json::json!({
                    "schema_version": 1,
                    "prompt": 2.5,
                    "completion": 8.0
                }))
            },
        )
        .await;

        assert_eq!(got.unwrap().completion, 8.0);
    }
}
