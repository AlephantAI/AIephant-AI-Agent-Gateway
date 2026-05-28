//! When `model` is omitted, choose default gateway model (`provider/model`)
//! from policy + price.

use std::{
    collections::{HashMap, HashSet},
    time::Instant,
};

use uuid::Uuid;

use crate::{
    app_state::AppState,
    default_model::{
        model_ids_from_config_json, model_ids_from_policy_overrides_json,
        pick_greatest_by_price_and_name, price_sum_from_info,
    },
    error::{api::ApiError, invalid_req::InvalidRequestError},
    middleware::model_support::split_provider_model,
    store::router::RouterStore,
    types::extensions::{AuthContext, VkPolicy},
    virtual_key::model_policy::model_access_allowed,
};

#[derive(Debug, Clone)]
struct DefaultModelCandidate {
    original: String,
    provider_code: String,
    model_id: String,
}

#[derive(Debug, Clone, PartialEq)]
struct ScoredGatewayModel {
    gateway_model: String,
    provider_code: String,
    price_sum: f64,
}

/// `path` is a Unified API subpath, e.g. `chat/completions`, `embeddings`
/// (matches `UnifiedApi` segments).
pub async fn choose_default_gateway_model(
    app_state: &AppState,
    path: &str,
    auth: &AuthContext,
    vk_policy: &VkPolicy,
) -> Result<String, ApiError> {
    choose_default_gateway_model_excluding_provider(
        app_state, path, auth, vk_policy, None,
    )
    .await
}

pub async fn choose_default_gateway_model_excluding_provider(
    app_state: &AppState,
    path: &str,
    auth: &AuthContext,
    vk_policy: &VkPolicy,
    excluded_provider_code: Option<&str>,
) -> Result<String, ApiError> {
    let debug_unified = std::env::var_os("AI_GATEWAY_DEBUG_UNIFIED").is_some();
    let started_at = Instant::now();
    if debug_unified {
        tracing::info!(
            "[unified_api] choose_default_gateway_model: entered path={path} \
             virtual_key_id={}",
            vk_policy.virtual_key_id
        );
    }
    tracing::warn!(
        path = %path,
        virtual_key_id = %vk_policy.virtual_key_id,
        "choose_default_gateway_model: entered"
    );
    let Some(store) = app_state.router_store() else {
        return Err(InvalidRequestError::NoModelAvailable.into());
    };
    let workspace_id = *auth.org_id.as_ref();

    let resolve_started_at = Instant::now();
    let list =
        resolve_model_id_list(store, workspace_id, vk_policy, auth).await?;
    let resolve_elapsed_ms = resolve_started_at.elapsed().as_millis();
    let raw_candidate_count = list.len();
    let list = expand_policy_model_list(app_state, list);
    let expanded_candidate_count = list.len();
    if debug_unified {
        tracing::info!(
            "[unified_api] choose_default_gateway_model: \
             resolve_model_id_list_ms={resolve_elapsed_ms} \
             raw_candidates={raw_candidate_count} \
             expanded_candidates={expanded_candidate_count}"
        );
    }
    tracing::warn!(
        path = %path,
        virtual_key_id = %vk_policy.virtual_key_id,
        resolve_model_id_list_ms = resolve_elapsed_ms,
        raw_candidates = raw_candidate_count,
        expanded_candidates = expanded_candidate_count,
        "choose_default_gateway_model: candidates resolved"
    );
    if list.is_empty() {
        return Err(InvalidRequestError::NoModelAvailable.into());
    }

    let scored = resolve_scored_gateway_models(
        store,
        path,
        vk_policy,
        &list,
        debug_unified,
    )
    .await?;

    let Some(chosen) =
        pick_best_scored_gateway_model(&scored, excluded_provider_code)
    else {
        return Err(InvalidRequestError::NoModelAvailable.into());
    };
    let total_elapsed_ms = started_at.elapsed().as_millis();
    if debug_unified {
        tracing::info!(
            "[unified_api] choose_default_gateway_model: completed \
             total_ms={total_elapsed_ms} scored_candidates={} chosen={chosen}",
            scored.len()
        );
    }
    tracing::warn!(
        path = %path,
        virtual_key_id = %vk_policy.virtual_key_id,
        total_ms = total_elapsed_ms,
        scored_candidates = scored.len(),
        chosen = %chosen,
        "choose_default_gateway_model: completed"
    );
    Ok(chosen)
}

async fn resolve_scored_gateway_models(
    store: &RouterStore,
    path: &str,
    vk_policy: &VkPolicy,
    list: &[String],
    debug_unified: bool,
) -> Result<Vec<ScoredGatewayModel>, ApiError> {
    let blocked = vk_policy.blocked_models.as_deref();
    let mut scored: Vec<ScoredGatewayModel> = vec![];
    let mut candidates: Vec<DefaultModelCandidate> = vec![];

    for m in list {
        if !model_access_allowed(m, None, blocked) {
            if debug_unified {
                tracing::info!(
                    "[unified_api] choose_default_gateway_model: \
                     candidate={m} skipped=blocked_or_not_allowed"
                );
            }
            continue;
        }
        let Ok(parsed) = split_provider_model(m) else {
            if debug_unified {
                tracing::info!(
                    "[unified_api] choose_default_gateway_model: \
                     candidate={m} skipped=invalid_provider_model"
                );
            }
            continue;
        };
        candidates.push(DefaultModelCandidate {
            original: m.clone(),
            provider_code: parsed.provider_raw.to_string(),
            model_id: parsed.model_raw.to_string(),
        });
    }

    let batch_started_at = Instant::now();
    let selection_rows = store
        .get_gateway_model_selection_info_batch(
            &candidates
                .iter()
                .map(|candidate| {
                    (
                        candidate.provider_code.clone(),
                        candidate.model_id.clone(),
                    )
                })
                .collect::<Vec<_>>(),
        )
        .await?;
    let batch_elapsed_ms = batch_started_at.elapsed().as_millis();
    if debug_unified {
        tracing::info!(
            "[unified_api] choose_default_gateway_model: \
             selection_info_batch_ms={batch_elapsed_ms} \
             requested_candidates={} batch_hits={}",
            candidates.len(),
            selection_rows.len()
        );
    }
    tracing::warn!(
        path = %path,
        virtual_key_id = %vk_policy.virtual_key_id,
        selection_info_batch_ms = batch_elapsed_ms,
        requested_candidates = candidates.len(),
        batch_hits = selection_rows.len(),
        "choose_default_gateway_model: batch candidate metadata loaded"
    );

    let selection_info_by_key: HashMap<String, Option<serde_json::Value>> =
        selection_rows
            .into_iter()
            .map(|row| {
                (
                    candidate_lookup_key(&row.provider_code, &row.model_id),
                    row.info.map(|value| value.0),
                )
            })
            .collect();

    for candidate in &candidates {
        let candidate_started_at = Instant::now();
        let lookup_key =
            candidate_lookup_key(&candidate.provider_code, &candidate.model_id);
        let Some(info) = selection_info_by_key.get(&lookup_key) else {
            if debug_unified {
                tracing::info!(
                    "[unified_api] choose_default_gateway_model: candidate={} \
                     supported=false \
                     selection_info_batch_ms={batch_elapsed_ms}",
                    candidate.original
                );
            }
            continue;
        };
        if !model_info_allows_unified_path(info.as_ref(), path) {
            if debug_unified {
                let interaction_type =
                    model_interaction_type_from_info(info.as_ref())
                        .unwrap_or("unknown");
                tracing::info!(
                    "[unified_api] choose_default_gateway_model: candidate={} \
                     skipped=interaction_type_mismatch \
                     selection_info_batch_ms={batch_elapsed_ms} \
                     interaction_type={interaction_type}",
                    candidate.original
                );
            }
            continue;
        }
        let sum = info.as_ref().map_or(0.0, price_sum_from_info);
        if debug_unified {
            let interaction_type =
                model_interaction_type_from_info(info.as_ref())
                    .unwrap_or("missing");
            tracing::info!(
                "[unified_api] choose_default_gateway_model: candidate={} \
                 accepted=true selection_info_batch_ms={batch_elapsed_ms} \
                 total_candidate_ms={} interaction_type={interaction_type} \
                 price_sum={sum}",
                candidate.original,
                candidate_started_at.elapsed().as_millis(),
            );
        }
        scored.push(ScoredGatewayModel {
            gateway_model: candidate.original.clone(),
            provider_code: candidate.provider_code.clone(),
            price_sum: sum,
        });
    }

    Ok(scored)
}

fn pick_best_scored_gateway_model(
    scored: &[ScoredGatewayModel],
    excluded_provider_code: Option<&str>,
) -> Option<String> {
    let filtered: Vec<(String, f64)> = scored
        .iter()
        .filter(|candidate| {
            excluded_provider_code.is_none_or(|excluded| {
                !candidate.provider_code.eq_ignore_ascii_case(excluded)
            })
        })
        .map(|candidate| (candidate.gateway_model.clone(), candidate.price_sum))
        .collect();
    if filtered.is_empty() {
        None
    } else {
        Some(pick_greatest_by_price_and_name(&filtered).to_string())
    }
}

fn candidate_lookup_key(provider_code: &str, model_id: &str) -> String {
    format!(
        "{}/{}",
        provider_code.to_ascii_lowercase(),
        model_id.to_ascii_lowercase()
    )
}

/// Expand bare `model_id` from policy into `code/model`; entries that already
/// include `provider/` are re-normalized before deduping.
fn expand_policy_model_list(
    app_state: &AppState,
    list: Vec<String>,
) -> Vec<String> {
    let index = app_state.get_bare_model_expand_index();
    let mut out: Vec<String> = Vec::new();
    let mut seen_lower: HashSet<String> = HashSet::new();
    for raw in list {
        let m = raw.trim();
        if m.is_empty() {
            continue;
        }
        if let Ok(parsed) = split_provider_model(m) {
            let full = format!("{}/{}", parsed.provider_raw, parsed.model_raw);
            if seen_lower.insert(full.to_ascii_lowercase()) {
                out.push(full);
            }
        } else {
            for g in index.gateway_models_for_bare_id(m) {
                if seen_lower.insert(g.to_ascii_lowercase()) {
                    out.push(g);
                }
            }
        }
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModelInteractionSelectionPolicy {
    /// Include in candidates only when `model_interaction_type` is explicitly
    /// `chat`, `multimodal`, or `reasoning`.
    ChatMultimodalReasoning,
    EmbeddingOnly,
    LegacyCompatible,
}

fn selection_policy_for_unified_path(
    path: &str,
) -> ModelInteractionSelectionPolicy {
    match path {
        "chat/completions" | "messages" | "responses" => {
            ModelInteractionSelectionPolicy::ChatMultimodalReasoning
        }
        "embeddings" => ModelInteractionSelectionPolicy::EmbeddingOnly,
        _ => ModelInteractionSelectionPolicy::LegacyCompatible,
    }
}

fn model_interaction_type_from_info(
    info: Option<&serde_json::Value>,
) -> Option<&str> {
    info.and_then(|i| {
        i.get("model_interaction_type")
            .and_then(serde_json::Value::as_str)
    })
}

fn policy_allows_interaction_type(
    policy: ModelInteractionSelectionPolicy,
    interaction_type: Option<&str>,
) -> bool {
    match policy {
        ModelInteractionSelectionPolicy::ChatMultimodalReasoning => {
            matches!(
                interaction_type,
                Some("chat" | "multimodal" | "reasoning")
            )
        }
        ModelInteractionSelectionPolicy::EmbeddingOnly => {
            interaction_type == Some("embedding")
        }
        ModelInteractionSelectionPolicy::LegacyCompatible => {
            interaction_type != Some("embedding")
        }
    }
}

fn model_info_allows_unified_path(
    info: Option<&serde_json::Value>,
    path: &str,
) -> bool {
    let policy = selection_policy_for_unified_path(path);
    let interaction_type = model_interaction_type_from_info(info);
    policy_allows_interaction_type(policy, interaction_type)
}

async fn resolve_model_id_list(
    store: &RouterStore,
    workspace_id: Uuid,
    vk: &VkPolicy,
    auth: &AuthContext,
) -> Result<Vec<String>, ApiError> {
    if let Some(v) = nonempty_allow_list(vk.allowed_models.as_deref()) {
        return Ok(v);
    }

    if let Some(v) = load_overrides_list(
        store,
        workspace_id,
        vk.virtual_key_id,
        auth.department_id,
    )
    .await?
    {
        return Ok(v);
    }

    if let Some(row) = store
        .get_policy_config_model_access_for_workspace(workspace_id)
        .await?
        && let Some(ids) = model_ids_from_config_json(&row.config.0)
    {
        return Ok(ids);
    }

    Ok(vec![])
}

fn nonempty_allow_list(allowed: Option<&[String]>) -> Option<Vec<String>> {
    let v: Vec<String> = allowed?
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if v.is_empty() { None } else { Some(v) }
}

async fn load_overrides_list(
    store: &RouterStore,
    workspace_id: Uuid,
    virtual_key_id: Uuid,
    department_id: Uuid,
) -> Result<Option<Vec<String>>, ApiError> {
    if department_id != Uuid::nil()
        && let Some(row) = store
            .get_policy_overrides_by_workspace_and_department(
                workspace_id,
                department_id,
            )
            .await?
        && let Some(ids) =
            model_ids_from_policy_overrides_json(&row.overrides.0)
        && !ids.is_empty()
    {
        return Ok(Some(ids));
    }
    if let Some(row) = store
        .get_policy_overrides_for_virtual_key(workspace_id, virtual_key_id)
        .await?
        && let Some(ids) =
            model_ids_from_policy_overrides_json(&row.overrides.0)
        && !ids.is_empty()
    {
        return Ok(Some(ids));
    }
    if let Some(row) = store
        .get_policy_overrides_workspace_default(workspace_id)
        .await?
        && let Some(ids) =
            model_ids_from_policy_overrides_json(&row.overrides.0)
        && !ids.is_empty()
    {
        return Ok(Some(ids));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        ModelInteractionSelectionPolicy, ScoredGatewayModel,
        expand_policy_model_list, model_info_allows_unified_path,
        pick_best_scored_gateway_model, policy_allows_interaction_type,
        selection_policy_for_unified_path,
    };
    use crate::{
        app::build_test_app, config::Config,
        discover::router::BareModelExpandIndex,
    };

    async fn app_state_with_index(
        f: impl FnOnce(&mut BareModelExpandIndex),
    ) -> crate::app_state::AppState {
        let app = build_test_app(Config::default()).await.expect("test app");
        let mut index = BareModelExpandIndex::default();
        f(&mut index);
        app.state.set_bare_model_expand_index(index);
        app.state
    }

    #[tokio::test]
    async fn expand_bare_model_yields_all_gateway_names() {
        let state = app_state_with_index(|idx| {
            idx.push("openai", "gpt-4o");
            idx.push("groq", "gpt-4o");
        })
        .await;
        let out = expand_policy_model_list(&state, vec!["gpt-4o".to_string()]);
        assert_eq!(out.len(), 2, "{out:?}");
        assert!(out.contains(&"openai/gpt-4o".to_string()));
        assert!(out.contains(&"groq/gpt-4o".to_string()));
    }

    #[tokio::test]
    async fn expand_qualified_dedupes_by_case() {
        let state = app_state_with_index(|_| {}).await;
        let out = expand_policy_model_list(
            &state,
            vec!["openai/gpt-4o".to_string(), "OpenAI/gpt-4o".to_string()],
        );
        assert_eq!(out, vec!["openai/gpt-4o".to_string()]);
    }

    #[tokio::test]
    async fn expand_mixed_bare_and_qualified_dedupes() {
        let state = app_state_with_index(|idx| {
            idx.push("openai", "my-model");
        })
        .await;
        let out = expand_policy_model_list(
            &state,
            vec!["my-model".to_string(), "openai/my-model".to_string()],
        );
        assert_eq!(out, vec!["openai/my-model".to_string()]);
    }

    #[tokio::test]
    async fn expand_skips_empty_and_unknown_bare() {
        let state = app_state_with_index(|idx| {
            idx.push("openai", "x");
        })
        .await;
        let out = expand_policy_model_list(
            &state,
            vec!["  ".to_string(), "no-such-bare".to_string()],
        );
        assert!(out.is_empty());
    }

    #[test]
    fn chat_completions_accepts_chat_multimodal_reasoning_rejects_others() {
        assert!(model_info_allows_unified_path(
            Some(&json!({ "model_interaction_type": "chat" })),
            "chat/completions",
        ));
        assert!(model_info_allows_unified_path(
            Some(&json!({ "model_interaction_type": "multimodal" })),
            "chat/completions",
        ));
        assert!(model_info_allows_unified_path(
            Some(&json!({ "model_interaction_type": "reasoning" })),
            "chat/completions",
        ));
        assert!(!model_info_allows_unified_path(None, "chat/completions"));
        assert!(!model_info_allows_unified_path(
            Some(&json!({ "model_interaction_type": "embedding" })),
            "chat/completions",
        ));
        assert!(!model_info_allows_unified_path(
            Some(&json!({ "model_interaction_type": "audio" })),
            "chat/completions",
        ));
        assert!(!model_info_allows_unified_path(
            Some(&json!({ "model_interaction_type": "image" })),
            "chat/completions",
        ));
    }

    #[test]
    fn responses_and_messages_follow_chat_like_policy() {
        for path in ["responses", "messages"] {
            assert!(model_info_allows_unified_path(
                Some(&json!({ "model_interaction_type": "chat" })),
                path,
            ));
            assert!(model_info_allows_unified_path(
                Some(&json!({ "model_interaction_type": "multimodal" })),
                path,
            ));
            assert!(model_info_allows_unified_path(
                Some(&json!({ "model_interaction_type": "reasoning" })),
                path,
            ));
            assert!(!model_info_allows_unified_path(None, path));
            assert!(!model_info_allows_unified_path(
                Some(&json!({ "model_interaction_type": "embedding" })),
                path,
            ));
            assert!(!model_info_allows_unified_path(
                Some(&json!({ "model_interaction_type": "audio" })),
                path,
            ));
        }
    }

    #[test]
    fn embeddings_requires_explicit_embedding_type() {
        assert!(model_info_allows_unified_path(
            Some(&json!({ "model_interaction_type": "embedding" })),
            "embeddings",
        ));
        assert!(!model_info_allows_unified_path(None, "embeddings"));
        assert!(!model_info_allows_unified_path(
            Some(&json!({ "model_interaction_type": "chat" })),
            "embeddings",
        ));
        assert!(!model_info_allows_unified_path(
            Some(&json!({ "model_interaction_type": "multimodal" })),
            "embeddings",
        ));
    }

    #[test]
    fn unified_paths_map_to_expected_selection_policy() {
        assert_eq!(
            selection_policy_for_unified_path("chat/completions"),
            ModelInteractionSelectionPolicy::ChatMultimodalReasoning
        );
        assert_eq!(
            selection_policy_for_unified_path("messages"),
            ModelInteractionSelectionPolicy::ChatMultimodalReasoning
        );
        assert_eq!(
            selection_policy_for_unified_path("responses"),
            ModelInteractionSelectionPolicy::ChatMultimodalReasoning
        );
        assert_eq!(
            selection_policy_for_unified_path("embeddings"),
            ModelInteractionSelectionPolicy::EmbeddingOnly
        );
    }

    #[test]
    fn chat_like_policy_accepts_only_explicit_chat_multimodal_reasoning() {
        assert!(policy_allows_interaction_type(
            ModelInteractionSelectionPolicy::ChatMultimodalReasoning,
            Some("chat"),
        ));
        assert!(policy_allows_interaction_type(
            ModelInteractionSelectionPolicy::ChatMultimodalReasoning,
            Some("multimodal"),
        ));
        assert!(policy_allows_interaction_type(
            ModelInteractionSelectionPolicy::ChatMultimodalReasoning,
            Some("reasoning"),
        ));
        assert!(!policy_allows_interaction_type(
            ModelInteractionSelectionPolicy::ChatMultimodalReasoning,
            None,
        ));
        assert!(!policy_allows_interaction_type(
            ModelInteractionSelectionPolicy::ChatMultimodalReasoning,
            Some("embedding"),
        ));
        assert!(!policy_allows_interaction_type(
            ModelInteractionSelectionPolicy::ChatMultimodalReasoning,
            Some("audio"),
        ));
    }

    #[test]
    fn embedding_policy_accepts_only_embedding() {
        assert!(policy_allows_interaction_type(
            ModelInteractionSelectionPolicy::EmbeddingOnly,
            Some("embedding"),
        ));
        assert!(!policy_allows_interaction_type(
            ModelInteractionSelectionPolicy::EmbeddingOnly,
            None,
        ));
        assert!(!policy_allows_interaction_type(
            ModelInteractionSelectionPolicy::EmbeddingOnly,
            Some("chat"),
        ));
    }

    #[test]
    fn pick_best_model_skips_excluded_provider() {
        let chosen = pick_best_scored_gateway_model(
            &[
                ScoredGatewayModel {
                    gateway_model: "openai/gpt-5.4".to_string(),
                    provider_code: "openai".to_string(),
                    price_sum: 10.0,
                },
                ScoredGatewayModel {
                    gateway_model: "google/gemini-2.5-pro".to_string(),
                    provider_code: "google".to_string(),
                    price_sum: 8.0,
                },
            ],
            Some("openai"),
        );
        assert_eq!(chosen.as_deref(), Some("google/gemini-2.5-pro"));
    }

    #[test]
    fn pick_best_model_returns_none_when_excluded_provider_removes_all_candidates()
     {
        let chosen = pick_best_scored_gateway_model(
            &[ScoredGatewayModel {
                gateway_model: "openai/gpt-5.4".to_string(),
                provider_code: "openai".to_string(),
                price_sum: 10.0,
            }],
            Some("openai"),
        );
        assert!(chosen.is_none());
    }
}
