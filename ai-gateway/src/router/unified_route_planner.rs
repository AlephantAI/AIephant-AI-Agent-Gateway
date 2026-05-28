use anthropic_ai_sdk::types::message::CreateMessageParams;
use async_openai::types::{
    CreateCompletionRequest, CreateEmbeddingRequest, CreateImageRequest,
    ImageModel,
};
use bytes::Bytes;
use http::Extensions;

use crate::{
    app_state::AppState,
    error::{api::ApiError, invalid_req::InvalidRequestError},
    ide_adapation::{
        client_profile::ClientProfile,
        responses_ingress_normalize::{
            apply_responses_wire_normalize_for_client,
            responses_request_routing_fields,
        },
        unified_chat_completions_routing_model,
    },
    types::{extensions::AuthContext, provider::InferenceProvider},
    virtual_key::enforce::check_model_access as check_vk_model_access,
};

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct UnifiedRouteRequest {
    pub path: String,
    pub body: Bytes,
    pub extensions: http::Extensions,
    pub explicit_client_model: bool,
    pub client_profile: ClientProfile,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub struct RouteDecision {
    pub selected_provider: InferenceProvider,
    pub selected_model: String,
    pub out_body: Bytes,
    pub candidates: Vec<RouteCandidate>,
    pub policy_checked: bool,
    pub model_body_passthrough: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub struct RouteCandidate {
    pub provider: InferenceProvider,
    pub model: String,
    pub reason: RouteCandidateReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum RouteCandidateReason {
    SingleProviderMasterKey,
    ExplicitProviderModel,
    DefaultModel,
    CustomProviderBaseUrl,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct UnifiedRoutePlanner {
    #[allow(dead_code)]
    app_state: AppState,
}

impl UnifiedRoutePlanner {
    #[must_use]
    #[allow(dead_code)]
    pub fn new(app_state: AppState) -> Self {
        Self { app_state }
    }

    #[allow(dead_code)]
    pub fn resolve_provider_from_master_key(
        extensions: &http::Extensions,
    ) -> Result<InferenceProvider, ApiError> {
        let auth = extensions.get::<AuthContext>().ok_or_else(|| {
            InvalidRequestError::UnsupportedGatewayModel(
                "missing auth context for unified provider routing".to_string(),
            )
        })?;
        match auth.master_key_allowed_providers.as_deref() {
            Some([provider]) => Ok(provider.clone()),
            _ => Err(InvalidRequestError::UnsupportedGatewayModel(
                "master key must resolve to exactly one provider".to_string(),
            )
            .into()),
        }
    }

    #[allow(dead_code)]
    fn check_model_access(
        extensions: &mut Extensions,
        model: &str,
    ) -> Result<(), ApiError> {
        check_vk_model_access(extensions, model)
    }

    fn routing_model_for_path(
        path: &str,
        body: Bytes,
        client_profile: ClientProfile,
    ) -> Result<(String, Bytes), ApiError> {
        match path {
            "chat/completions" => {
                let model = unified_chat_completions_routing_model(&body)?;
                Ok((model, body))
            }
            "responses" => {
                let body = apply_responses_wire_normalize_for_client(
                    body,
                    client_profile,
                )?;
                let fields = responses_request_routing_fields(&body)?;
                Ok((fields.model, body))
            }
            "completions" => {
                let request =
                    serde_json::from_slice::<CreateCompletionRequest>(&body)
                        .map_err(InvalidRequestError::InvalidRequestBody)?;
                Ok((request.model, body))
            }
            "embeddings" => {
                let request =
                    serde_json::from_slice::<CreateEmbeddingRequest>(&body)
                        .map_err(InvalidRequestError::InvalidRequestBody)?;
                Ok((request.model, body))
            }
            "images/generations" => {
                let request =
                    serde_json::from_slice::<CreateImageRequest>(&body)
                        .map_err(InvalidRequestError::InvalidRequestBody)?;
                let model_s = request
                    .model
                    .as_ref()
                    .ok_or(InvalidRequestError::MissingModelId)?;
                let model = Self::image_model_routing_name(model_s);
                if model.is_empty() {
                    return Err(ApiError::Internal(
                        crate::error::internal::InternalError::MapperError(
                            crate::error::mapper::MapperError::InvalidModelName(
                                "Model name cannot be empty".to_string(),
                            ),
                        ),
                    ));
                }
                Ok((model, body))
            }
            "messages" => {
                let request =
                    serde_json::from_slice::<CreateMessageParams>(&body)
                        .map_err(InvalidRequestError::InvalidRequestBody)?;
                Ok((request.model, body))
            }
            _ => {
                Err(InvalidRequestError::UnsupportedEndpoint(path.to_string())
                    .into())
            }
        }
    }

    fn image_model_routing_name(model: &ImageModel) -> String {
        match model {
            ImageModel::DallE2 => "dall-e-2".to_string(),
            ImageModel::DallE3 => "dall-e-3".to_string(),
            ImageModel::Other(model) => model.clone(),
        }
    }

    #[allow(dead_code)]
    pub fn plan(
        &self,
        request: UnifiedRouteRequest,
    ) -> Result<RouteDecision, ApiError> {
        let UnifiedRouteRequest {
            path,
            body,
            mut extensions,
            explicit_client_model,
            client_profile,
        } = request;
        let (selected_model, out_body) =
            Self::routing_model_for_path(&path, body, client_profile)?;

        if let Err(error) =
            Self::check_model_access(&mut extensions, &selected_model)
        {
            self.app_state.0.metrics.vk.model_denied.add(1, &[]);
            tracing::debug!(
                model = %selected_model,
                "virtual key model access denied"
            );
            return Err(error);
        }

        let selected_provider =
            Self::resolve_provider_from_master_key(&extensions)?;

        Ok(RouteDecision {
            selected_provider: selected_provider.clone(),
            selected_model: selected_model.clone(),
            out_body,
            candidates: vec![RouteCandidate {
                provider: selected_provider,
                model: selected_model,
                reason: RouteCandidateReason::SingleProviderMasterKey,
            }],
            policy_checked: true,
            model_body_passthrough: explicit_client_model,
        })
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use serde_json::json;
    use uuid::Uuid;

    use super::{
        RouteCandidateReason, UnifiedRoutePlanner, UnifiedRouteRequest,
    };
    use crate::types::{
        extensions::{AuthContext, VkPolicy},
        org::OrgId,
        provider::InferenceProvider,
        secret::Secret,
        user::UserId,
    };

    fn auth_context_with_allowed(
        providers: Option<Vec<InferenceProvider>>,
    ) -> AuthContext {
        AuthContext {
            api_key: Secret::from("sk-test".to_string()),
            user_id: UserId::new(Uuid::new_v4()),
            org_id: OrgId::new(Uuid::new_v4()),
            virtual_key_id: Some(Uuid::new_v4()),
            virtual_key_prefix: "vk-test".to_string(),
            master_key_id: Some(Uuid::new_v4()),
            master_key_base_url: None,
            department_id: Uuid::nil(),
            entity_type: String::new(),
            entity_id: Uuid::nil(),
            entity_name: String::new(),
            body_ttl_days: 90,
            is_custom_provider: false,
            master_key_allowed_providers: providers,
        }
    }

    fn unsupported_gateway_model_message(
        err: crate::error::api::ApiError,
    ) -> String {
        match err {
            crate::error::api::ApiError::InvalidRequest(
                crate::error::invalid_req::InvalidRequestError::UnsupportedGatewayModel(message),
            ) => message,
            other => panic!("expected UnsupportedGatewayModel, got {other:?}"),
        }
    }

    async fn planner() -> UnifiedRoutePlanner {
        let app = crate::app::build_test_app(crate::config::Config::default())
            .await
            .expect("build app");
        UnifiedRoutePlanner::new(app.state)
    }

    fn route_request(
        path: &str,
        body: serde_json::Value,
        explicit_client_model: bool,
    ) -> UnifiedRouteRequest {
        let mut extensions = http::Extensions::new();
        extensions.insert(auth_context_with_allowed(Some(vec![
            InferenceProvider::OpenAI,
        ])));

        UnifiedRouteRequest {
            path: path.to_string(),
            body: Bytes::from(serde_json::to_vec(&body).unwrap()),
            extensions,
            explicit_client_model,
            client_profile:
                crate::ide_adapation::client_profile::ClientProfile::Unknown,
        }
    }

    #[test]
    fn resolves_single_openai_provider_from_master_key() {
        let mut extensions = http::Extensions::new();
        extensions.insert(auth_context_with_allowed(Some(vec![
            InferenceProvider::OpenAI,
        ])));

        let provider =
            UnifiedRoutePlanner::resolve_provider_from_master_key(&extensions)
                .expect("single OpenAI provider should resolve");

        assert_eq!(provider, InferenceProvider::OpenAI);
    }

    #[test]
    fn resolves_single_anthropic_provider_from_master_key() {
        let mut extensions = http::Extensions::new();
        extensions.insert(auth_context_with_allowed(Some(vec![
            InferenceProvider::Anthropic,
        ])));

        let provider =
            UnifiedRoutePlanner::resolve_provider_from_master_key(&extensions)
                .expect("single Anthropic provider should resolve");

        assert_eq!(provider, InferenceProvider::Anthropic);
    }

    #[test]
    fn rejects_missing_empty_or_multiple_provider_list() {
        for providers in [
            None,
            Some(vec![]),
            Some(vec![
                InferenceProvider::OpenAI,
                InferenceProvider::Anthropic,
            ]),
        ] {
            let mut extensions = http::Extensions::new();
            extensions.insert(auth_context_with_allowed(providers));

            let err = UnifiedRoutePlanner::resolve_provider_from_master_key(
                &extensions,
            )
            .expect_err("provider should not resolve");

            assert_eq!(
                unsupported_gateway_model_message(err),
                "master key must resolve to exactly one provider"
            );
        }
    }

    #[test]
    fn rejects_missing_auth_context() {
        let extensions = http::Extensions::new();

        let err =
            UnifiedRoutePlanner::resolve_provider_from_master_key(&extensions)
                .expect_err("provider should not resolve");

        assert_eq!(
            unsupported_gateway_model_message(err),
            "missing auth context for unified provider routing"
        );
    }

    #[tokio::test]
    async fn plans_chat_completions_single_provider_decision() {
        let planner = planner().await;
        let body = json!({
            "model": "openai/gpt-5.4",
            "messages": [{"role": "user", "content": "hi"}]
        });
        let request = route_request("chat/completions", body.clone(), true);

        let decision = planner.plan(request).expect("plan should succeed");

        assert_eq!(decision.selected_provider, InferenceProvider::OpenAI);
        assert_eq!(decision.selected_model, "openai/gpt-5.4");
        assert_eq!(
            decision.out_body,
            Bytes::from(serde_json::to_vec(&body).unwrap())
        );
        assert!(decision.policy_checked);
        assert!(decision.model_body_passthrough);
        assert_eq!(decision.candidates.len(), 1);
        assert_eq!(decision.candidates[0].provider, InferenceProvider::OpenAI);
        assert_eq!(decision.candidates[0].model, "openai/gpt-5.4");
        assert_eq!(
            decision.candidates[0].reason,
            RouteCandidateReason::SingleProviderMasterKey
        );
    }

    #[tokio::test]
    async fn plans_responses_after_wire_normalize() {
        let planner = planner().await;
        let request = route_request(
            "responses",
            json!({
                "model": "openai/gpt-5.5",
                "input": "hi",
                "tools": [{ "type": "web_search" }]
            }),
            true,
        );

        let decision = planner.plan(request).expect("plan should succeed");
        let out: serde_json::Value =
            serde_json::from_slice(&decision.out_body).unwrap();

        assert_eq!(decision.selected_model, "openai/gpt-5.5");
        assert_eq!(out["tools"][0]["type"], "web_search_preview");
    }

    #[tokio::test]
    async fn plans_embeddings_single_provider_decision() {
        let planner = planner().await;
        let body = json!({
            "model": "openai/text-embedding-3-small",
            "input": "hi"
        });
        let request = route_request("embeddings", body.clone(), true);

        let decision = planner.plan(request).expect("plan should succeed");

        assert_eq!(decision.selected_provider, InferenceProvider::OpenAI);
        assert_eq!(decision.selected_model, "openai/text-embedding-3-small");
        assert_eq!(
            decision.out_body,
            Bytes::from(serde_json::to_vec(&body).unwrap())
        );
        assert!(decision.policy_checked);
        assert_eq!(
            decision.candidates[0].reason,
            RouteCandidateReason::SingleProviderMasterKey
        );
    }

    #[tokio::test]
    async fn plans_image_generations_single_provider_decision() {
        let planner = planner().await;
        let body = json!({
            "model": "gpt-image-1",
            "prompt": "draw a small lighthouse"
        });
        let request = route_request("images/generations", body.clone(), true);

        let decision = planner.plan(request).expect("plan should succeed");

        assert_eq!(decision.selected_provider, InferenceProvider::OpenAI);
        assert_eq!(decision.selected_model, "gpt-image-1");
        assert_eq!(
            decision.out_body,
            Bytes::from(serde_json::to_vec(&body).unwrap())
        );
        assert!(decision.policy_checked);
        assert_eq!(
            decision.candidates[0].reason,
            RouteCandidateReason::SingleProviderMasterKey
        );
    }

    #[tokio::test]
    async fn image_generations_missing_model_errors_before_route_decision() {
        let planner = planner().await;
        let request = route_request(
            "images/generations",
            json!({
                "prompt": "draw a small lighthouse"
            }),
            true,
        );

        let err = planner
            .plan(request)
            .expect_err("missing model should error");

        assert!(matches!(
            err,
            crate::error::api::ApiError::InvalidRequest(
                crate::error::invalid_req::InvalidRequestError::MissingModelId
            )
        ));
    }

    #[tokio::test]
    async fn image_generations_empty_model_errors_before_route_decision() {
        let planner = planner().await;
        let request = route_request(
            "images/generations",
            json!({
                "model": "",
                "prompt": "draw a small lighthouse"
            }),
            true,
        );

        let err = planner.plan(request).expect_err("empty model should error");

        assert!(matches!(
            err,
            crate::error::api::ApiError::Internal(
                crate::error::internal::InternalError::MapperError(
                    crate::error::mapper::MapperError::InvalidModelName(message)
                )
            ) if message == "Model name cannot be empty"
        ));
    }

    #[tokio::test]
    async fn plans_messages_single_provider_decision() {
        let planner = planner().await;
        let body = json!({
            "model": "claude-sonnet-4-5",
            "max_tokens": 32,
            "messages": [{"role": "user", "content": "hi"}]
        });
        let request = route_request("messages", body.clone(), true);

        let decision = planner.plan(request).expect("plan should succeed");

        assert_eq!(decision.selected_provider, InferenceProvider::OpenAI);
        assert_eq!(decision.selected_model, "claude-sonnet-4-5");
        assert_eq!(
            decision.out_body,
            Bytes::from(serde_json::to_vec(&body).unwrap())
        );
        assert!(decision.policy_checked);
        assert!(decision.model_body_passthrough);
        assert_eq!(
            decision.candidates[0].reason,
            RouteCandidateReason::SingleProviderMasterKey
        );
    }

    #[tokio::test]
    async fn default_model_decision_does_not_enable_body_passthrough() {
        let planner = planner().await;
        let request = route_request(
            "completions",
            json!({
                "model": "openai/gpt-5.4",
                "prompt": "hi"
            }),
            false,
        );

        let decision = planner.plan(request).expect("plan should succeed");

        assert!(decision.policy_checked);
        assert!(!decision.model_body_passthrough);
    }

    #[tokio::test]
    async fn blocked_model_policy_denies_before_route_decision() {
        let planner = planner().await;
        let mut extensions = http::Extensions::new();
        extensions.insert(VkPolicy {
            virtual_key_id: Uuid::new_v4(),
            allowed_models: None,
            blocked_models: Some(vec!["openai/gpt-5.4".to_string()]),
        });
        let request = UnifiedRouteRequest {
            path: "chat/completions".to_string(),
            body: Bytes::from(
                serde_json::to_vec(&json!({
                    "model": "openai/gpt-5.4",
                    "messages": [{"role": "user", "content": "hi"}]
                }))
                .unwrap(),
            ),
            extensions,
            explicit_client_model: true,
            client_profile:
                crate::ide_adapation::client_profile::ClientProfile::Unknown,
        };

        let err = planner.plan(request).expect_err("model should be denied");

        assert!(matches!(
            err,
            crate::error::api::ApiError::InvalidRequest(
                crate::error::invalid_req::InvalidRequestError::ModelAccessDenied(_)
            )
        ));
    }
}
