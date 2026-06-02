use std::time::Duration;

use bytes::Bytes;
use chrono::{DateTime, Utc};
use http::{HeaderMap, StatusCode, header};
use http_body_util::BodyExt;
use indexmap::IndexMap;
use opentelemetry::KeyValue;
use reqwest::Client;
use tokio::{sync::oneshot, time::Instant};
use typed_builder::TypedBuilder;
use url::Url;
use uuid::Uuid;

use super::model_info::ModelInfo;
use crate::{
    agent::{context::AgentContext, name::resolve_agent_name},
    app_state::AppState,
    config::deployment_target::DeploymentTarget,
    error::{init::InitError, logger::LoggerError},
    logger::usage_parse::usage_counts_from_response_body_for_log,
    metrics::tfft::TFFTFuture,
    session_headers::{SessionHeaders, inject_session_properties},
    types::{
        body::BodyReader,
        extensions::{
            AuthContext, LargeContextDecision, MapperContext,
            PromptCompressionTokenPair, PromptContext,
            PromptHeaderForRequestLog,
        },
        logger::{
            AiGatewayBodyMapping, AlephantLogMetadata, Log, LogMessage,
            RequestLog, ResponseLog,
        },
        provider::InferenceProvider,
        router::RouterId,
    },
    utils::debug_log::DebugLogConfig,
};

const ALEPHANT_HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

#[inline]
fn nonempty_string_opt(s: &str) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s.to_owned())
    }
}

fn parse_ai_gateway_body_mapping(
    raw: Option<&String>,
) -> Option<AiGatewayBodyMapping> {
    let s = raw.map_or("", String::as_str).trim();
    if s.is_empty() {
        return None;
    }
    match s.to_uppercase().as_str() {
        "OPENAI" => Some(AiGatewayBodyMapping::Openai),
        "NO_MAPPING" => Some(AiGatewayBodyMapping::NoMapping),
        "RESPONSES" => Some(AiGatewayBodyMapping::Responses),
        _ => None,
    }
}

fn inference_provider_for_ingest_meta(
    provider: &InferenceProvider,
) -> Option<String> {
    match provider {
        InferenceProvider::OpenAI => Some("openai".to_string()),
        InferenceProvider::Anthropic => Some("anthropic".to_string()),
        InferenceProvider::Bedrock => Some("bedrock".to_string()),
        InferenceProvider::GoogleGemini => Some("google-ai-studio".to_string()),
        InferenceProvider::Ollama
        | InferenceProvider::Custom
        | InferenceProvider::Named(_) => None,
    }
}

fn header_optional_string(headers: &HeaderMap, key: &str) -> Option<String> {
    headers
        .get(key)
        .and_then(|v| v.to_str().ok())
        .map(std::borrow::ToOwned::to_owned)
}

fn extract_request_properties(
    headers: &HeaderMap,
    session_ctx: Option<&SessionHeaders>,
) -> IndexMap<String, String> {
    let mut properties = IndexMap::new();
    for (name, value) in headers {
        if name.as_str().starts_with("alephant-property-")
            && let Ok(value_str) = value.to_str()
        {
            properties.insert(name.to_string(), value_str.to_string());
        }
    }
    if let Some(session_ctx) = session_ctx {
        inject_session_properties(&mut properties, session_ctx);
    }
    properties
}

#[derive(Debug, Clone, Default)]
struct AgentLogFields {
    alephant_agent_id: Option<String>,
    self_reported_agent_name: Option<String>,
    alephant_agent_name: Option<String>,
    alephant_agent_name_source: Option<String>,
    alephant_agent_uid: Option<Uuid>,
    alephant_run_id: Option<String>,
    alephant_step_id: Option<String>,
    alephant_parent_step_id: Option<String>,
    alephant_tool_call_id: Option<String>,
    alephant_handoff_id: Option<String>,
    alephant_graph_node: Option<String>,
    alephant_iteration: Option<u32>,
    alephant_state_hash: Option<String>,
    alephant_step_kind: Option<String>,
    alephant_step_source: Option<String>,
    alephant_step_confidence: Option<String>,
    alephant_agent_trust_level: Option<String>,
}

fn clone_nonempty(value: &Option<String>) -> Option<String> {
    value.as_deref().and_then(nonempty_string_opt)
}

fn agent_log_fields(agent_ctx: Option<&AgentContext>) -> AgentLogFields {
    let Some(agent_ctx) = agent_ctx else {
        return AgentLogFields::default();
    };

    AgentLogFields {
        alephant_agent_id: clone_nonempty(&agent_ctx.agent_id_external),
        self_reported_agent_name: clone_nonempty(&agent_ctx.agent_name),
        alephant_agent_name: None,
        alephant_agent_name_source: None,
        alephant_agent_uid: agent_ctx.agent_uid,
        alephant_run_id: clone_nonempty(&agent_ctx.run_id),
        alephant_step_id: clone_nonempty(&agent_ctx.step_id),
        alephant_parent_step_id: clone_nonempty(&agent_ctx.parent_step_id),
        alephant_tool_call_id: clone_nonempty(&agent_ctx.tool_call_id),
        alephant_handoff_id: clone_nonempty(&agent_ctx.handoff_id),
        alephant_graph_node: clone_nonempty(&agent_ctx.graph_node),
        alephant_iteration: agent_ctx.iteration,
        alephant_state_hash: clone_nonempty(&agent_ctx.state_hash),
        alephant_step_kind: agent_ctx
            .step_kind
            .map(|kind| kind.as_str().to_string()),
        alephant_step_source: Some(agent_ctx.step_source.as_str().to_string()),
        alephant_step_confidence: Some(
            agent_ctx.step_confidence.as_str().to_string(),
        ),
        alephant_agent_trust_level: Some(
            agent_ctx.trust_level.as_str().to_string(),
        ),
    }
}

fn apply_final_agent_name_to_request_log(
    registered_agent_name: Option<&str>,
    agent_fields: &mut AgentLogFields,
    properties: &mut IndexMap<String, String>,
) {
    let resolved_agent_name = resolve_agent_name(
        registered_agent_name,
        None,
        agent_fields.self_reported_agent_name.as_deref(),
    );
    agent_fields.alephant_agent_name = resolved_agent_name.name;
    agent_fields.alephant_agent_name_source =
        resolved_agent_name.source.map(str::to_string);
    if let Some(trust_level) = resolved_agent_name.trust_level {
        agent_fields.alephant_agent_trust_level =
            Some(trust_level.as_str().to_string());
    }
    if let Some(conflict) = resolved_agent_name.conflict {
        properties.insert(
            "registeredAgentName".to_string(),
            conflict.registered_agent_name,
        );
        properties.insert(
            "selfReportedAgentName".to_string(),
            conflict.self_reported_agent_name,
        );
        properties.insert(
            "selfReportedAgentNameSource".to_string(),
            conflict.self_reported_agent_name_source.to_string(),
        );
        properties.insert("agentNameConflict".to_string(), "true".to_string());
    }
}

fn resolved_response_cost(
    _model_info: Option<&ModelInfo>,
    _usage: &crate::types::usage_tokens::UsageTokenCounts,
) -> f64 {
    0.0
}

#[derive(Debug)]
pub struct AlephantHttpClient {
    pub request_client: Client,
}

impl AlephantHttpClient {
    pub fn new() -> Result<Self, InitError> {
        Ok(Self {
            request_client: Client::builder()
                .tcp_nodelay(true)
                .connect_timeout(ALEPHANT_HTTP_CONNECT_TIMEOUT)
                .build()
                .map_err(InitError::CreateReqwestClient)?,
        })
    }
}

#[derive(Debug, TypedBuilder)]
pub struct LoggerService {
    app_state: AppState,
    auth_ctx: AuthContext,
    start_time: DateTime<Utc>,
    start_instant: Instant,
    response_body: BodyReader,
    request_body: Bytes,
    target_url: Url,
    request_headers: HeaderMap,
    response_status: StatusCode,
    provider: InferenceProvider,
    mapper_ctx: MapperContext,
    router_id: Option<RouterId>,
    deployment_target: DeploymentTarget,
    tfft_rx: oneshot::Receiver<()>,
    request_id: Uuid,
    response_id: Uuid,
    /// When upstream response headers were received (before body streaming).
    response_created_at: DateTime<Utc>,
    #[builder(default)]
    cache_enabled: Option<bool>,
    #[builder(default)]
    cache_bucket_max_size: Option<u8>,
    #[builder(default)]
    cache_control: Option<String>,
    #[builder(default)]
    cache_reference_id: Option<String>,
    #[builder(default)]
    prompt_ctx: Option<PromptContext>,
    #[builder(default)]
    prompt_header_for_request_log: Option<PromptHeaderForRequestLog>,
    #[builder(default)]
    large_context_decision: Option<LargeContextDecision>,
    #[builder(default)]
    prompt_compression_tokens: Option<PromptCompressionTokenPair>,
    #[builder(default)]
    session_ctx: Option<SessionHeaders>,
    #[builder(default)]
    agent_ctx: Option<AgentContext>,
    #[builder(default)]
    ai_gateway_body_mapping: Option<String>,
    #[builder(default = DebugLogConfig::from_env())]
    debug_log_config: DebugLogConfig,
}

impl LoggerService {
    fn build_alephant_metadata(
        &mut self,
        model: &str,
    ) -> Result<AlephantLogMetadata, LoggerError> {
        let mut alephant_metadata = AlephantLogMetadata::from_headers(
            &mut self.request_headers,
            self.router_id.clone(),
            &self.deployment_target,
            self.prompt_ctx.clone(),
        )?;
        alephant_metadata.gateway_model = Some(model.to_string());
        alephant_metadata.gateway_provider =
            inference_provider_for_ingest_meta(&self.provider);
        alephant_metadata.provider_model_id =
            self.mapper_ctx.model.as_ref().map(ToString::to_string);
        if let Some(ref decision) = self.large_context_decision {
            alephant_metadata.apply_large_context_decision(decision);
        }
        alephant_metadata.is_passthrough_billing = Some(true);
        alephant_metadata.ai_gateway_body_mapping =
            parse_ai_gateway_body_mapping(
                self.ai_gateway_body_mapping.as_ref(),
            );
        Ok(alephant_metadata)
    }

    #[tracing::instrument(skip_all)]
    #[allow(clippy::cast_precision_loss, clippy::too_many_lines)]
    pub async fn log(mut self) -> Result<(), LoggerError> {
        tracing::trace!("logging request");
        let model = self
            .mapper_ctx
            .model
            .as_ref()
            .map_or_else(|| "unknown".to_string(), ToString::to_string);
        let alephant_metadata = self.build_alephant_metadata(&model)?;
        let tfft_future = TFFTFuture::new(self.start_instant, self.tfft_rx);
        let collect_future = self.response_body.collect();
        let (response_body, tfft_duration) =
            tokio::join!(collect_future, tfft_future);
        let response_body = response_body
            .inspect_err(|_| tracing::error!("infallible errored"))
            .expect("infallible never errors")
            .to_bytes();
        let target = self.target_url.to_string();
        tracing::info!(
            target_url = %target,
            response_len = response_body.len(),
            is_stream = self.mapper_ctx.is_stream,
            "dispatcher response collected"
        );
        if self.debug_log_config.body {
            let preview =
                crate::utils::debug_log::debug_body_preview(&response_body);
            tracing::info!(
                target_url = %target,
                body_len = preview.body_len,
                truncated = preview.truncated,
                body = %preview.body,
                "dispatcher response body (debug body enabled)"
            );
        }
        let tfft_duration = tfft_duration.unwrap_or_else(|_| {
            tracing::warn!("Failed to get TFFT signal");
            Duration::from_secs(0)
        });
        tracing::trace!(tfft_duration = ?tfft_duration, "tfft_duration");
        let req_body_len = self.request_body.len();
        let resp_body_len = response_body.len();
        let usage_counts = usage_counts_from_response_body_for_log(
            self.mapper_ctx.is_stream,
            &response_body,
        );
        let origin_prompt_tokens = self
            .prompt_compression_tokens
            .as_ref()
            .map_or(usage_counts.prompt_tokens, |p| {
                i64::from(p.origin_prompt_token)
            });
        let response_cost = resolved_response_cost(None, &usage_counts);
        let country_code =
            header_optional_string(&self.request_headers, "cf-ipcountry")
                .or_else(|| {
                    header_optional_string(
                        &self.request_headers,
                        "x-alephant-country-code",
                    )
                });
        let request_referrer = self
            .request_headers
            .get(header::REFERER)
            .and_then(|v| v.to_str().ok())
            .map(std::borrow::ToOwned::to_owned);
        let (
            request_body_str,
            response_body_str,
            body_ttl_days,
            storage_location,
        ) = crate::logger::cloud_bodies::resolve_cloud_log_bodies(
            &self.app_state.0.s3,
            self.auth_ctx.body_ttl_days,
            self.request_id,
            self.auth_ctx.org_id,
            &self.request_body,
            &response_body,
        )
        .await?;

        let attributes = [
            KeyValue::new("provider", self.provider.to_string()),
            KeyValue::new("model", model.clone()),
            KeyValue::new("path", self.target_url.path().to_string()),
        ];
        if self.mapper_ctx.is_stream {
            self.app_state
                .0
                .metrics
                .tfft_duration
                .record(tfft_duration.as_millis() as f64, &attributes);
        }

        let req_path = self.target_url.path().to_string();
        let provider = match self.provider {
            InferenceProvider::Ollama => "CUSTOM".to_string(),
            InferenceProvider::GoogleGemini => "GOOGLE".to_string(),
            provider => provider.to_string().to_uppercase(),
        };

        let mut properties = extract_request_properties(
            &self.request_headers,
            self.session_ctx.as_ref(),
        );
        let mut agent_fields = agent_log_fields(self.agent_ctx.as_ref());
        apply_final_agent_name_to_request_log(
            self.auth_ctx.registered_agent_name.as_deref(),
            &mut agent_fields,
            &mut properties,
        );

        let completed_at = Utc::now();
        let latency_ms =
            (completed_at - self.start_time).num_milliseconds().max(0);
        let log_response_created_at = self.response_created_at;
        let tfft_ms = if self.mapper_ctx.is_stream {
            i64::try_from(tfft_duration.as_millis()).unwrap_or(i64::MAX)
        } else {
            0
        };

        let (prompt_id, prompt_version) =
            if let Some(ref h) = self.prompt_header_for_request_log {
                (Some(h.prompt_id.clone()), h.prompt_version.clone())
            } else if let Some(ref ctx) = self.prompt_ctx {
                (Some(ctx.prompt_id.clone()), ctx.prompt_version_id.clone())
            } else {
                (None, None)
            };

        let ai_mapping_internal = self
            .ai_gateway_body_mapping
            .as_ref()
            .map_or_else(String::new, std::string::ToString::to_string);

        let department_id = self.auth_ctx.department_id;
        let alephant_department_name = if department_id == Uuid::nil() {
            None
        } else if let Some(store) = self.app_state.router_store() {
            match store.fetch_department_name_by_id(department_id).await {
                Ok(Some(name)) => {
                    nonempty_string_opt(name.trim()).or(Some(String::new()))
                }
                Ok(None) => Some(String::new()),
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        %department_id,
                        "request log: department name lookup failed"
                    );
                    None
                }
            }
        } else {
            None
        };

        let request_log = RequestLog::builder()
            .id(self.request_id)
            .user_id(self.auth_ctx.user_id)
            .workspace_id(self.auth_ctx.org_id)
            .workspace_type(self.auth_ctx.workspace_type.clone())
            .session_id(
                self.session_ctx
                    .as_ref()
                    .map(|session| session.session_id.clone()),
            )
            .prompt_id(prompt_id)
            .prompt_version(prompt_version)
            .properties(properties)
            .alephant_agent_id(agent_fields.alephant_agent_id)
            .alephant_agent_name(agent_fields.alephant_agent_name)
            .alephant_agent_name_source(agent_fields.alephant_agent_name_source)
            .alephant_agent_uid(agent_fields.alephant_agent_uid)
            .alephant_run_id(agent_fields.alephant_run_id)
            .alephant_step_id(agent_fields.alephant_step_id)
            .alephant_parent_step_id(agent_fields.alephant_parent_step_id)
            .alephant_tool_call_id(agent_fields.alephant_tool_call_id)
            .alephant_handoff_id(agent_fields.alephant_handoff_id)
            .alephant_graph_node(agent_fields.alephant_graph_node)
            .alephant_iteration(agent_fields.alephant_iteration)
            .alephant_state_hash(agent_fields.alephant_state_hash)
            .alephant_step_kind(agent_fields.alephant_step_kind)
            .alephant_step_source(agent_fields.alephant_step_source)
            .alephant_step_confidence(agent_fields.alephant_step_confidence)
            .alephant_agent_trust_level(agent_fields.alephant_agent_trust_level)
            .alephant_virtual_key_id(self.auth_ctx.virtual_key_id)
            .alephant_master_key_id(self.auth_ctx.master_key_id)
            .alephant_virtual_key_name(nonempty_string_opt(
                &self.auth_ctx.entity_name,
            ))
            .alephant_virtual_key_prefix(nonempty_string_opt(
                &self.auth_ctx.virtual_key_prefix,
            ))
            .alephant_department_name(alephant_department_name)
            .department_id(department_id)
            .entity_type(self.auth_ctx.entity_type.clone())
            .entity_id(self.auth_ctx.entity_id)
            .entity_name(self.auth_ctx.entity_name.clone())
            .target_url(self.target_url)
            .provider(provider)
            .model(model.clone())
            .body_size(req_body_len as f64)
            .path(req_path)
            .country_code(country_code)
            .request_referrer(request_referrer)
            .request_created_at(self.start_time)
            .is_stream(self.mapper_ctx.is_stream)
            .request_body(request_body_str)
            .body_ttl_days(body_ttl_days)
            .storage_location(storage_location)
            .ai_gateway_body_mapping(ai_mapping_internal)
            .updated_at(Some(completed_at))
            .threat(Some(false))
            .assets(Vec::new()) // placeholder, no value
            .scores(IndexMap::new()) // placeholder, no value
            .cache_enabled(self.cache_enabled) // whether cache is enabled
            .cache_bucket_max_size(self.cache_bucket_max_size) // max cache bucket size
            .cache_control(self.cache_control) // e.g. max-age=3600,public,no-cache,no-store,must-revalidate
            .cache_reference_id(self.cache_reference_id) // cache reference id
            .build();
        let response_log = ResponseLog::builder()
            .id(self.response_id)
            .status(i64::from(self.response_status.as_u16()))
            .body_size(resp_body_len as f64)
            .latency(latency_ms)
            .time_to_first_token(tfft_ms)
            .response_created_at(log_response_created_at)
            .response_body(response_body_str)
            .model(Some(model))
            .origin_prompt_tokens(origin_prompt_tokens)
            .prompt_tokens(usage_counts.prompt_tokens)
            .completion_tokens(usage_counts.completion_tokens)
            .prompt_cache_write_tokens(usage_counts.prompt_cache_write_tokens)
            .prompt_cache_read_tokens(usage_counts.prompt_cache_read_tokens)
            .prompt_audio_tokens(usage_counts.prompt_audio_tokens)
            .completion_audio_tokens(usage_counts.completion_audio_tokens)
            .reasoning_tokens(usage_counts.reasoning_tokens)
            .cost(response_cost)
            .is_passthrough_billing(true) // placeholder, no value
            .build();
        let log = Log::new(request_log, response_log);
        let log_message = LogMessage::builder()
            .authorization(self.auth_ctx.api_key.expose().clone())
            .alephant_meta(alephant_metadata)
            .log(log)
            .build();

        let auth = self.auth_ctx.api_key.expose();
        let auth_preview: String = auth.chars().take(8).collect();
        tracing::debug!(
            authorization_preview = %format!("{auth_preview}..."),
            request_id = %self.request_id,
            large_context_handler = ?self
                .large_context_decision
                .as_ref()
                .map(|decision| decision.handler.as_str()),
            large_context_action = ?self
                .large_context_decision
                .as_ref()
                .map(|decision| decision.action.as_str()),
            "delivering request log via configured transport",
        );

        self.app_state
            .request_log_transport()
            .send(&log_message)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use http::{HeaderMap, HeaderValue};

    use super::{super::model_info::ModelInfo, extract_request_properties};
    use crate::{
        session_headers::{
            ALEPHANT_SESSION_ID_PROPERTY, ALEPHANT_SESSION_NAME_PROPERTY,
            ALEPHANT_SESSION_PATH_PROPERTY, SessionHeaders,
        },
        types::{
            extensions::PromptCompressionTokenPair,
            usage_tokens::UsageTokenCounts,
        },
    };

    #[test]
    fn extract_request_properties_includes_session_properties() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "alephant-property-custom",
            HeaderValue::from_static("keep"),
        );
        let session = SessionHeaders {
            session_id: "session-123".to_string(),
            session_path: Some("/workflow/step-1".to_string()),
            session_name: Some("Planner".to_string()),
        };

        let properties = extract_request_properties(&headers, Some(&session));

        assert_eq!(
            properties
                .get("alephant-property-custom")
                .map(String::as_str),
            Some("keep")
        );
        assert_eq!(
            properties
                .get(ALEPHANT_SESSION_ID_PROPERTY)
                .map(String::as_str),
            Some("session-123")
        );
        assert_eq!(
            properties
                .get(ALEPHANT_SESSION_PATH_PROPERTY)
                .map(String::as_str),
            Some("/workflow/step-1")
        );
        assert_eq!(
            properties
                .get(ALEPHANT_SESSION_NAME_PROPERTY)
                .map(String::as_str),
            Some("Planner")
        );
    }

    #[test]
    fn resolved_response_cost_returns_zero_when_model_info_missing() {
        let usage = UsageTokenCounts {
            prompt_tokens: 100,
            completion_tokens: 50,
            ..UsageTokenCounts::default()
        };

        let got = super::resolved_response_cost(None, &usage);

        assert_eq!(got, 0.0);
    }

    #[test]
    fn resolved_response_cost_remains_zero_even_when_model_info_exists() {
        let usage = UsageTokenCounts {
            prompt_tokens: 1_000,
            completion_tokens: 500,
            ..UsageTokenCounts::default()
        };
        let info = ModelInfo {
            schema_version: 1,
            prompt: 3e-6,
            completion: 12e-6,
            input_cache_read: None,
            tag: None,
            create_time: None,
            max_context_tokens: None,
            max_completion_tokens: None,
            model_interaction_type: None,
        };

        let got = super::resolved_response_cost(Some(&info), &usage);

        assert_eq!(got, 0.0);
    }

    #[test]
    fn origin_prompt_tokens_matches_usage_without_compression() {
        let pair: Option<PromptCompressionTokenPair> = None;
        let usage = UsageTokenCounts {
            prompt_tokens: 100,
            ..Default::default()
        };
        let got = pair
            .as_ref()
            .map_or(usage.prompt_tokens, |p| i64::from(p.origin_prompt_token));
        assert_eq!(got, 100);
    }

    #[test]
    fn origin_prompt_tokens_prefers_compression_pre_estimate() {
        let pair = Some(PromptCompressionTokenPair {
            origin_prompt_token: 4_096,
            compression_prompt_token: 2_048,
        });
        let usage = UsageTokenCounts {
            prompt_tokens: 100,
            ..Default::default()
        };
        let got = pair
            .as_ref()
            .map_or(usage.prompt_tokens, |p| i64::from(p.origin_prompt_token));
        assert_eq!(got, 4_096);
    }
}

#[cfg(test)]
mod agent_property_tests {
    use indexmap::IndexMap;
    use uuid::Uuid;

    use crate::agent::context::{
        AgentConfidence, AgentContext, AgentStepKind, AgentStepSource,
        AgentTrustLevel,
    };

    #[test]
    fn agent_log_fields_extracts_standard_keys_without_properties() {
        let mut properties = IndexMap::new();
        properties
            .insert("alephant-property-custom".to_string(), "keep".to_string());
        let agent_uid = Uuid::parse_str("01890f5a-52fd-7b9a-b51e-33a22f7b6f24")
            .expect("static uuid is valid");
        let ctx = AgentContext {
            agent_id_external: Some("coding-agent".to_string()),
            agent_name: Some("Support Bot".to_string()),
            agent_uid: Some(agent_uid),
            run_id: Some("run-1".to_string()),
            step_id: Some("step-1".to_string()),
            parent_step_id: Some("step-0".to_string()),
            tool_call_id: Some("call-1".to_string()),
            handoff_id: Some("handoff-1".to_string()),
            graph_node: Some("planner".to_string()),
            iteration: Some(3),
            state_hash: Some("sha256:abc".to_string()),
            step_kind: Some(AgentStepKind::Planning),
            step_source: AgentStepSource::Runtime,
            step_confidence: AgentConfidence::High,
            trust_level: AgentTrustLevel::SelfReported,
            ..AgentContext::default()
        };

        let fields = super::agent_log_fields(Some(&ctx));

        assert_eq!(fields.alephant_agent_id.as_deref(), Some("coding-agent"));
        assert_eq!(
            fields.self_reported_agent_name.as_deref(),
            Some("Support Bot")
        );
        assert_eq!(fields.alephant_agent_uid, Some(agent_uid));
        assert_eq!(fields.alephant_run_id.as_deref(), Some("run-1"));
        assert_eq!(fields.alephant_step_id.as_deref(), Some("step-1"));
        assert_eq!(fields.alephant_parent_step_id.as_deref(), Some("step-0"));
        assert_eq!(fields.alephant_tool_call_id.as_deref(), Some("call-1"));
        assert_eq!(fields.alephant_handoff_id.as_deref(), Some("handoff-1"));
        assert_eq!(fields.alephant_graph_node.as_deref(), Some("planner"));
        assert_eq!(fields.alephant_iteration, Some(3));
        assert_eq!(fields.alephant_state_hash.as_deref(), Some("sha256:abc"));
        assert_eq!(fields.alephant_step_kind.as_deref(), Some("planning"));
        assert_eq!(fields.alephant_step_source.as_deref(), Some("runtime"));
        assert_eq!(fields.alephant_step_confidence.as_deref(), Some("high"));
        assert_eq!(
            fields.alephant_agent_trust_level.as_deref(),
            Some("self_reported")
        );
        assert_eq!(
            properties
                .get("alephant-property-custom")
                .map(String::as_str),
            Some("keep")
        );
        assert!(!properties.contains_key("Alephant-Agent-Id"));
    }

    #[test]
    fn request_log_agent_name_prefers_registered_name_and_records_conflict() {
        let mut fields = super::AgentLogFields {
            self_reported_agent_name: Some("External Bot".to_string()),
            alephant_agent_trust_level: Some("self_reported".to_string()),
            ..super::AgentLogFields::default()
        };
        let mut properties = IndexMap::new();

        super::apply_final_agent_name_to_request_log(
            Some("Support Bot"),
            &mut fields,
            &mut properties,
        );

        assert_eq!(fields.alephant_agent_name.as_deref(), Some("Support Bot"));
        assert_eq!(
            fields.alephant_agent_name_source.as_deref(),
            Some("virtual_key_label")
        );
        assert_eq!(
            fields.alephant_agent_trust_level.as_deref(),
            Some("auth_bound")
        );
        assert_eq!(
            properties.get("registeredAgentName").map(String::as_str),
            Some("Support Bot")
        );
        assert_eq!(
            properties.get("selfReportedAgentName").map(String::as_str),
            Some("External Bot")
        );
        assert_eq!(
            properties
                .get("selfReportedAgentNameSource")
                .map(String::as_str),
            Some("self_reported_header")
        );
        assert_eq!(
            properties.get("agentNameConflict").map(String::as_str),
            Some("true")
        );
    }

    #[test]
    fn request_log_agent_name_uses_header_name_when_unregistered() {
        let mut fields = super::AgentLogFields {
            self_reported_agent_name: Some("External Bot".to_string()),
            alephant_agent_trust_level: Some("self_reported".to_string()),
            ..super::AgentLogFields::default()
        };
        let mut properties = IndexMap::new();

        super::apply_final_agent_name_to_request_log(
            None,
            &mut fields,
            &mut properties,
        );

        assert_eq!(fields.alephant_agent_name.as_deref(), Some("External Bot"));
        assert_eq!(
            fields.alephant_agent_name_source.as_deref(),
            Some("self_reported_header")
        );
        assert_eq!(
            fields.alephant_agent_trust_level.as_deref(),
            Some("self_reported")
        );
        assert!(properties.get("agentNameConflict").is_none());
    }

    #[test]
    fn request_log_agent_name_stays_empty_when_no_source_exists() {
        let mut fields = super::AgentLogFields::default();
        let mut properties = IndexMap::new();

        super::apply_final_agent_name_to_request_log(
            None,
            &mut fields,
            &mut properties,
        );

        assert_eq!(fields.alephant_agent_name, None);
        assert_eq!(fields.alephant_agent_name_source, None);
        assert_eq!(fields.alephant_agent_trust_level, None);
        assert!(properties.is_empty());
    }
}
