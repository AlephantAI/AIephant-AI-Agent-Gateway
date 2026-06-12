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
    agent::{
        context::{
            AgentConfidence, AgentContext, AgentEventPhase, AgentEventSourceTrust, AgentPolicyMode,
            AgentPolicyStage, AgentStepKind, AgentStepSource,
        },
        event::{AgentEventEnvelope, AgentEventSource},
        name::resolve_agent_name,
        sink::emit_agent_event,
        tool_observer::{
            ObservedChatCompletionStreamToolCall, ObservedResponsesItem, ObservedResponsesItemKind,
            ObservedResponsesStreamItem, ObservedResponsesStreamItemKind, ObservedToolCall,
            observe_chat_completion_stream_tool_calls, observe_chat_completion_tool_calls,
            observe_responses_nonstream_agent_items, observe_responses_stream_agent_items,
        },
    },
    app_state::AppState,
    config::deployment_target::DeploymentTarget,
    error::{init::InitError, logger::LoggerError},
    logger::usage_parse::usage_counts_from_response_body_for_log,
    metrics::tfft::TFFTFuture,
    session_headers::{SessionHeaders, inject_session_properties},
    types::{
        body::BodyReader,
        extensions::{
            AuthContext, ClientResponseSemantic, LargeContextDecision, LoggerResponseWireSemantic,
            MapperContext, PromptCompressionTokenPair, PromptContext, PromptHeaderForRequestLog,
        },
        logger::{
            AiGatewayBodyMapping, AlephantLogMetadata, Log, LogMessage, RequestLog, ResponseLog,
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

fn parse_ai_gateway_body_mapping(raw: Option<&String>) -> Option<AiGatewayBodyMapping> {
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

fn should_observe_agent_response(
    agent_enabled: bool,
    is_stream: bool,
    agent_ctx: Option<&AgentContext>,
) -> bool {
    if !agent_enabled || is_stream {
        return false;
    }
    let Some(ctx) = agent_ctx else {
        return false;
    };
    ctx.agent_id_external.is_some() || ctx.agent_uid.is_some() || ctx.run_id.is_some()
}

fn should_observe_chat_completion_tool_calls(
    agent_enabled: bool,
    is_stream: bool,
    response_semantic: ClientResponseSemantic,
    agent_ctx: Option<&AgentContext>,
) -> bool {
    response_semantic == ClientResponseSemantic::ChatCompletions
        && should_observe_agent_response(agent_enabled, is_stream, agent_ctx)
}

fn should_observe_chat_completion_stream_tool_calls(
    agent_enabled: bool,
    is_stream: bool,
    client_response_semantic: ClientResponseSemantic,
    logger_response_wire_semantic: LoggerResponseWireSemantic,
    unified_responses_bridge_chat_completions_sse: bool,
    cursor_responses_via_chat_completions: bool,
    client_expects_responses_wire: bool,
    agent_ctx: Option<&AgentContext>,
) -> bool {
    is_stream
        && !unified_responses_bridge_chat_completions_sse
        && !cursor_responses_via_chat_completions
        && !client_expects_responses_wire
        && client_response_semantic == ClientResponseSemantic::ChatCompletions
        && logger_response_wire_semantic == LoggerResponseWireSemantic::ChatCompletionsSse
        && should_observe_agent_response(agent_enabled, false, agent_ctx)
}

fn should_observe_responses_agent_items(
    agent_enabled: bool,
    is_stream: bool,
    response_semantic: ClientResponseSemantic,
    agent_ctx: Option<&AgentContext>,
) -> bool {
    response_semantic == ClientResponseSemantic::Responses
        && should_observe_agent_response(agent_enabled, is_stream, agent_ctx)
}

fn should_observe_responses_stream_agent_items(
    agent_enabled: bool,
    is_stream: bool,
    client_response_semantic: ClientResponseSemantic,
    logger_response_wire_semantic: LoggerResponseWireSemantic,
    agent_ctx: Option<&AgentContext>,
) -> bool {
    is_stream
        && client_response_semantic == ClientResponseSemantic::Responses
        && logger_response_wire_semantic == LoggerResponseWireSemantic::ResponsesSse
        && should_observe_agent_response(agent_enabled, false, agent_ctx)
}

fn observed_tool_call_envelope(
    ctx: &AgentContext,
    observed: &ObservedToolCall,
    request_id: Uuid,
    provider: &str,
    model: &str,
    policy_mode: &str,
    alephant_agent_name: Option<&str>,
    alephant_agent_name_source: Option<&str>,
    alephant_agent_trust_level: Option<&str>,
) -> AgentEventEnvelope {
    let policy_mode = policy_mode
        .parse::<AgentPolicyMode>()
        .expect("AgentPolicyMode parser is infallible");
    let metadata = serde_json::json!({
        "observer": "chat_completions_tool_observer",
        "provider": provider,
        "model": model,
        "request_id": request_id.to_string(),
        "tool_type": observed.tool_type,
        "arguments_summary": observed.arguments_summary,
        "choice_index": observed.choice_index,
    });

    AgentEventEnvelope {
        version: "2026-05-27".to_string(),
        event_id: format!("evt_{}", Uuid::new_v4().simple()),
        event_type: "tool.call.observed".to_string(),
        event_source: AgentEventSource::Alephant,
        event_phase: AgentEventPhase::After,
        policy_stage: AgentPolicyStage::AuditOnly,
        policy_mode,
        event_source_trust: AgentEventSourceTrust::GatewayObserved,
        sequence: None,
        observed_at: Utc::now(),
        timestamp: None,
        name: Some(observed.tool_name.clone()),
        alephant_agent_name: alephant_agent_name.map(str::to_string),
        alephant_agent_name_source: alephant_agent_name_source.map(str::to_string),
        alephant_agent_trust_level: alephant_agent_trust_level.map(str::to_string),
        workspace_id: String::new(),
        virtual_key_id: None,
        agent_id_external: ctx.agent_id_external.clone(),
        agent_uid: ctx.agent_uid,
        run_id: ctx.run_id.clone(),
        step_id: ctx.step_id.clone(),
        parent_step_id: ctx.parent_step_id.clone(),
        tool_call_id: observed.tool_call_id.clone(),
        handoff_id: ctx.handoff_id.clone(),
        graph_node: ctx.graph_node.clone(),
        step_kind: Some(AgentStepKind::ToolCall),
        step_source: AgentStepSource::Gateway,
        step_confidence: AgentConfidence::Medium,
        trust_level: ctx.trust_level,
        context_conflict: false,
        step_id_conflict: false,
        attempt: None,
        input_hash: None,
        metadata,
        billing_mirror_trusted: false,
    }
}

fn observed_chat_stream_step_id(
    request_id: Uuid,
    observed: &ObservedChatCompletionStreamToolCall,
) -> String {
    let key = observed
        .tool_call_index
        .map(|index| format!("index_{index}"))
        .or_else(|| {
            observed
                .tool_call_id
                .as_ref()
                .map(|id| format!("call_{id}"))
        })
        .unwrap_or_else(|| "unknown".to_string());

    format!(
        "gwobs:chatcmpl:{request_id}:{}:{key}:tool_call_observed",
        observed.choice_index
    )
}

fn observed_chat_stream_tool_call_envelope(
    ctx: &AgentContext,
    observed: &ObservedChatCompletionStreamToolCall,
    request_id: Uuid,
    provider: &str,
    model: &str,
    policy_mode: &str,
    alephant_agent_name: Option<&str>,
    alephant_agent_name_source: Option<&str>,
    alephant_agent_trust_level: Option<&str>,
) -> AgentEventEnvelope {
    let policy_mode = policy_mode
        .parse::<AgentPolicyMode>()
        .expect("AgentPolicyMode parser is infallible");
    let mut metadata = if observed.metadata.is_object() {
        observed.metadata.clone()
    } else {
        serde_json::json!({})
    };
    if let Some(map) = metadata.as_object_mut() {
        map.insert("provider".to_string(), serde_json::json!(provider));
        map.insert("model".to_string(), serde_json::json!(model));
        map.insert(
            "request_id".to_string(),
            serde_json::json!(request_id.to_string()),
        );
    }

    AgentEventEnvelope {
        version: "2026-05-27".to_string(),
        event_id: format!("evt_{}", Uuid::new_v4().simple()),
        event_type: "tool.call.observed".to_string(),
        event_source: AgentEventSource::Alephant,
        event_phase: AgentEventPhase::After,
        policy_stage: AgentPolicyStage::AuditOnly,
        policy_mode,
        event_source_trust: AgentEventSourceTrust::GatewayObserved,
        sequence: None,
        observed_at: Utc::now(),
        timestamp: None,
        name: Some(
            observed
                .tool_name
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
        ),
        alephant_agent_name: alephant_agent_name.map(str::to_string),
        alephant_agent_name_source: alephant_agent_name_source.map(str::to_string),
        alephant_agent_trust_level: alephant_agent_trust_level.map(str::to_string),
        workspace_id: String::new(),
        virtual_key_id: None,
        agent_id_external: ctx.agent_id_external.clone(),
        agent_uid: ctx.agent_uid,
        run_id: ctx.run_id.clone(),
        step_id: Some(observed_chat_stream_step_id(request_id, observed)),
        parent_step_id: ctx.step_id.clone(),
        tool_call_id: observed.tool_call_id.clone(),
        handoff_id: ctx.handoff_id.clone(),
        graph_node: ctx.graph_node.clone(),
        step_kind: Some(AgentStepKind::ToolCall),
        step_source: AgentStepSource::Gateway,
        step_confidence: AgentConfidence::Medium,
        trust_level: ctx.trust_level,
        context_conflict: false,
        step_id_conflict: false,
        attempt: None,
        input_hash: None,
        metadata,
        billing_mirror_trusted: false,
    }
}

fn observed_stream_step_id(request_id: Uuid, observed: &ObservedResponsesStreamItem) -> String {
    let response_id = observed.response_id.as_deref().unwrap_or("unknown");
    let output_index_or_sequence = observed
        .output_index
        .map(|index| index.to_string())
        .unwrap_or_else(|| observed.sequence.to_string());
    let item_id = observed.item_id.as_deref().unwrap_or("none");
    let event_type = observed.event_type.replace('.', "_");

    format!(
        "gwobs:responses:{request_id}:{response_id}:\
         {output_index_or_sequence}:{item_id}:{event_type}"
    )
}

fn observed_responses_item_envelope(
    ctx: &AgentContext,
    observed: &ObservedResponsesItem,
    request_id: Uuid,
    provider: &str,
    model: &str,
    policy_mode: &str,
    alephant_agent_name: Option<&str>,
    alephant_agent_name_source: Option<&str>,
    alephant_agent_trust_level: Option<&str>,
) -> AgentEventEnvelope {
    let policy_mode = policy_mode
        .parse::<AgentPolicyMode>()
        .expect("AgentPolicyMode parser is infallible");
    let mut metadata = observed.metadata.clone();
    if let Some(map) = metadata.as_object_mut() {
        map.insert("provider".to_string(), serde_json::json!(provider));
        map.insert("model".to_string(), serde_json::json!(model));
        map.insert(
            "request_id".to_string(),
            serde_json::json!(request_id.to_string()),
        );
        map.insert(
            "response_id".to_string(),
            serde_json::json!(observed.response_id),
        );
        map.insert(
            "output_index".to_string(),
            serde_json::json!(observed.output_index),
        );
        map.insert("item_id".to_string(), serde_json::json!(observed.item_id));
        map.insert("call_id".to_string(), serde_json::json!(observed.call_id));
    }

    let step_kind = match observed.kind {
        ObservedResponsesItemKind::FunctionCall | ObservedResponsesItemKind::McpCall => {
            Some(AgentStepKind::ToolCall)
        }
        ObservedResponsesItemKind::Reasoning => Some(AgentStepKind::Reasoning),
    };

    AgentEventEnvelope {
        version: "2026-05-27".to_string(),
        event_id: format!("evt_{}", Uuid::new_v4().simple()),
        event_type: observed.event_type.to_string(),
        event_source: AgentEventSource::Alephant,
        event_phase: AgentEventPhase::After,
        policy_stage: AgentPolicyStage::AuditOnly,
        policy_mode,
        event_source_trust: AgentEventSourceTrust::GatewayObserved,
        sequence: None,
        observed_at: Utc::now(),
        timestamp: None,
        name: observed.name.clone(),
        alephant_agent_name: alephant_agent_name.map(str::to_string),
        alephant_agent_name_source: alephant_agent_name_source.map(str::to_string),
        alephant_agent_trust_level: alephant_agent_trust_level.map(str::to_string),
        workspace_id: String::new(),
        virtual_key_id: None,
        agent_id_external: ctx.agent_id_external.clone(),
        agent_uid: ctx.agent_uid,
        run_id: ctx.run_id.clone(),
        step_id: ctx.step_id.clone(),
        parent_step_id: ctx.parent_step_id.clone(),
        tool_call_id: observed.call_id.clone(),
        handoff_id: ctx.handoff_id.clone(),
        graph_node: ctx.graph_node.clone(),
        step_kind,
        step_source: AgentStepSource::Gateway,
        step_confidence: if ctx.step_id.is_some() {
            AgentConfidence::Medium
        } else {
            AgentConfidence::Low
        },
        trust_level: ctx.trust_level,
        context_conflict: false,
        step_id_conflict: false,
        attempt: None,
        input_hash: None,
        metadata,
        billing_mirror_trusted: false,
    }
}

fn observed_responses_stream_item_envelope(
    ctx: &AgentContext,
    observed: &ObservedResponsesStreamItem,
    request_id: Uuid,
    provider: &str,
    model: &str,
    policy_mode: &str,
    alephant_agent_name: Option<&str>,
    alephant_agent_name_source: Option<&str>,
    alephant_agent_trust_level: Option<&str>,
) -> AgentEventEnvelope {
    let policy_mode = policy_mode
        .parse::<AgentPolicyMode>()
        .expect("AgentPolicyMode parser is infallible");
    let mut metadata = if observed.metadata.is_object() {
        observed.metadata.clone()
    } else {
        serde_json::json!({})
    };
    if let Some(map) = metadata.as_object_mut() {
        map.insert("provider".to_string(), serde_json::json!(provider));
        map.insert("model".to_string(), serde_json::json!(model));
        map.insert(
            "request_id".to_string(),
            serde_json::json!(request_id.to_string()),
        );
        map.insert(
            "response_id".to_string(),
            serde_json::json!(observed.response_id),
        );
        map.insert(
            "output_index".to_string(),
            serde_json::json!(observed.output_index),
        );
        map.insert("item_id".to_string(), serde_json::json!(observed.item_id));
        map.insert("call_id".to_string(), serde_json::json!(observed.call_id));
    }

    let step_kind = match observed.kind {
        ObservedResponsesStreamItemKind::FunctionCall
        | ObservedResponsesStreamItemKind::McpCall => Some(AgentStepKind::ToolCall),
        ObservedResponsesStreamItemKind::Reasoning => Some(AgentStepKind::Reasoning),
        ObservedResponsesStreamItemKind::ResponseCompleted => Some(AgentStepKind::LlmCall),
        ObservedResponsesStreamItemKind::Error => Some(AgentStepKind::ErrorRecovery),
    };

    AgentEventEnvelope {
        version: "2026-05-27".to_string(),
        event_id: format!("evt_{}", Uuid::new_v4().simple()),
        event_type: observed.event_type.to_string(),
        event_source: AgentEventSource::Alephant,
        event_phase: AgentEventPhase::After,
        policy_stage: AgentPolicyStage::AuditOnly,
        policy_mode,
        event_source_trust: AgentEventSourceTrust::GatewayObserved,
        sequence: Some(u64::from(observed.sequence)),
        observed_at: Utc::now(),
        timestamp: None,
        name: observed.name.clone(),
        alephant_agent_name: alephant_agent_name.map(str::to_string),
        alephant_agent_name_source: alephant_agent_name_source.map(str::to_string),
        alephant_agent_trust_level: alephant_agent_trust_level.map(str::to_string),
        workspace_id: String::new(),
        virtual_key_id: None,
        agent_id_external: ctx.agent_id_external.clone(),
        agent_uid: ctx.agent_uid,
        run_id: ctx.run_id.clone(),
        step_id: Some(observed_stream_step_id(request_id, observed)),
        parent_step_id: ctx.step_id.clone(),
        tool_call_id: observed.call_id.clone(),
        handoff_id: ctx.handoff_id.clone(),
        graph_node: ctx.graph_node.clone(),
        step_kind,
        step_source: AgentStepSource::Gateway,
        step_confidence: AgentConfidence::Medium,
        trust_level: ctx.trust_level,
        context_conflict: false,
        step_id_conflict: false,
        attempt: None,
        input_hash: None,
        metadata,
        billing_mirror_trusted: false,
    }
}

fn apply_auth_scope_to_observed_tool_event(
    envelope: &mut AgentEventEnvelope,
    workspace_id: impl ToString,
    virtual_key_id: Option<Uuid>,
) {
    envelope.workspace_id = workspace_id.to_string();
    envelope.virtual_key_id = virtual_key_id;
}

fn inference_provider_for_ingest_meta(provider: &InferenceProvider) -> Option<String> {
    match provider {
        InferenceProvider::OpenAI => Some("openai".to_string()),
        InferenceProvider::Anthropic => Some("anthropic".to_string()),
        InferenceProvider::Bedrock => Some("bedrock".to_string()),
        InferenceProvider::GoogleGemini => Some("google-ai-studio".to_string()),
        InferenceProvider::Ollama | InferenceProvider::Custom | InferenceProvider::Named(_) => None,
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
        alephant_step_kind: agent_ctx.step_kind.map(|kind| kind.as_str().to_string()),
        alephant_step_source: Some(agent_ctx.step_source.as_str().to_string()),
        alephant_step_confidence: Some(agent_ctx.step_confidence.as_str().to_string()),
        alephant_agent_trust_level: Some(agent_ctx.trust_level.as_str().to_string()),
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
    agent_fields.alephant_agent_name_source = resolved_agent_name.source.map(str::to_string);
    if let Some(trust_level) = resolved_agent_name.trust_level {
        agent_fields.alephant_agent_trust_level = Some(trust_level.as_str().to_string());
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
    fn build_alephant_metadata(&mut self, model: &str) -> Result<AlephantLogMetadata, LoggerError> {
        let mut alephant_metadata = AlephantLogMetadata::from_headers(
            &mut self.request_headers,
            self.router_id.clone(),
            &self.deployment_target,
            self.prompt_ctx.clone(),
        )?;
        alephant_metadata.gateway_model = Some(model.to_string());
        alephant_metadata.gateway_provider = inference_provider_for_ingest_meta(&self.provider);
        alephant_metadata.provider_model_id =
            self.mapper_ctx.model.as_ref().map(ToString::to_string);
        if let Some(ref decision) = self.large_context_decision {
            alephant_metadata.apply_large_context_decision(decision);
        }
        alephant_metadata.is_passthrough_billing = Some(true);
        alephant_metadata.ai_gateway_body_mapping =
            parse_ai_gateway_body_mapping(self.ai_gateway_body_mapping.as_ref());
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
        let (response_body, tfft_duration) = tokio::join!(collect_future, tfft_future);
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
            let preview = crate::utils::debug_log::debug_body_preview(&response_body);
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
        let usage_counts =
            usage_counts_from_response_body_for_log(self.mapper_ctx.is_stream, &response_body);
        let origin_prompt_tokens = self
            .prompt_compression_tokens
            .as_ref()
            .map_or(usage_counts.prompt_tokens, |p| {
                i64::from(p.origin_prompt_token)
            });
        let response_cost = resolved_response_cost(None, &usage_counts);
        let country_code = header_optional_string(&self.request_headers, "cf-ipcountry")
            .or_else(|| header_optional_string(&self.request_headers, "x-alephant-country-code"));
        let request_referrer = self
            .request_headers
            .get(header::REFERER)
            .and_then(|v| v.to_str().ok())
            .map(std::borrow::ToOwned::to_owned);
        let (request_body_str, response_body_str, body_ttl_days, storage_location) =
            crate::logger::cloud_bodies::resolve_cloud_log_bodies(
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

        let mut properties =
            extract_request_properties(&self.request_headers, self.session_ctx.as_ref());
        let mut agent_fields = agent_log_fields(self.agent_ctx.as_ref());
        apply_final_agent_name_to_request_log(
            self.auth_ctx.registered_agent_name.as_deref(),
            &mut agent_fields,
            &mut properties,
        );
        if should_observe_chat_completion_tool_calls(
            self.app_state.config().agent.enabled,
            self.mapper_ctx.is_stream,
            self.mapper_ctx.client_response_semantic,
            self.agent_ctx.as_ref(),
        ) && let Some(agent_ctx) = self.agent_ctx.as_ref()
        {
            let observed_tool_calls = observe_chat_completion_tool_calls(&response_body);
            for observed in observed_tool_calls {
                let mut envelope = observed_tool_call_envelope(
                    agent_ctx,
                    &observed,
                    self.request_id,
                    &provider,
                    &model,
                    self.app_state.config().agent.policy_mode.as_str(),
                    agent_fields.alephant_agent_name.as_deref(),
                    agent_fields.alephant_agent_name_source.as_deref(),
                    agent_fields.alephant_agent_trust_level.as_deref(),
                );
                apply_auth_scope_to_observed_tool_event(
                    &mut envelope,
                    self.auth_ctx.org_id,
                    self.auth_ctx.virtual_key_id,
                );
                if let Err(err) = emit_agent_event(&self.app_state, &self.auth_ctx, &envelope).await
                {
                    tracing::warn!(
                        error = %err,
                        request_id = %self.request_id,
                        tool_name = %observed.tool_name,
                        "failed to emit observed agent tool call event"
                    );
                }
            }
        }

        let completed_at = Utc::now();
        let latency_ms = (completed_at - self.start_time).num_milliseconds().max(0);
        let log_response_created_at = self.response_created_at;
        let tfft_ms = if self.mapper_ctx.is_stream {
            i64::try_from(tfft_duration.as_millis()).unwrap_or(i64::MAX)
        } else {
            0
        };

        let (prompt_id, prompt_version) = if let Some(ref h) = self.prompt_header_for_request_log {
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
                Ok(Some(name)) => nonempty_string_opt(name.trim()).or(Some(String::new())),
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

        let provider_for_event = provider.clone();
        let model_for_event = model.clone();
        let agent_fields_for_event = agent_fields.clone();

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
            .alephant_virtual_key_name(nonempty_string_opt(&self.auth_ctx.entity_name))
            .alephant_virtual_key_prefix(nonempty_string_opt(&self.auth_ctx.virtual_key_prefix))
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
        if should_observe_chat_completion_stream_tool_calls(
            self.app_state.config().agent.enabled,
            self.mapper_ctx.is_stream,
            self.mapper_ctx.client_response_semantic,
            self.mapper_ctx.logger_response_wire_semantic,
            self.mapper_ctx
                .unified_responses_bridge_chat_completions_sse,
            self.mapper_ctx.cursor_responses_via_chat_completions,
            self.mapper_ctx.client_expects_responses_wire,
            self.agent_ctx.as_ref(),
        ) && let Some(agent_ctx) = self.agent_ctx.as_ref()
        {
            let observed_tool_calls =
                observe_chat_completion_stream_tool_calls(self.request_id, &response_body);
            for observed in observed_tool_calls {
                let mut envelope = observed_chat_stream_tool_call_envelope(
                    agent_ctx,
                    &observed,
                    self.request_id,
                    &provider_for_event,
                    &model_for_event,
                    self.app_state.config().agent.policy_mode.as_str(),
                    agent_fields_for_event.alephant_agent_name.as_deref(),
                    agent_fields_for_event.alephant_agent_name_source.as_deref(),
                    agent_fields_for_event.alephant_agent_trust_level.as_deref(),
                );
                apply_auth_scope_to_observed_tool_event(
                    &mut envelope,
                    self.auth_ctx.org_id,
                    self.auth_ctx.virtual_key_id,
                );
                if let Err(err) = emit_agent_event(&self.app_state, &self.auth_ctx, &envelope).await
                {
                    tracing::warn!(
                        error = %err,
                        request_id = %self.request_id,
                        tool_call_id = ?observed.tool_call_id,
                        tool_name = ?observed.tool_name,
                        "failed to emit observed chat completions stream tool call event"
                    );
                }
            }
        }
        if should_observe_responses_stream_agent_items(
            self.app_state.config().agent.enabled,
            self.mapper_ctx.is_stream,
            self.mapper_ctx.client_response_semantic,
            self.mapper_ctx.logger_response_wire_semantic,
            self.agent_ctx.as_ref(),
        ) && let Some(agent_ctx) = self.agent_ctx.as_ref()
        {
            let observed_items =
                observe_responses_stream_agent_items(self.request_id, &response_body);
            for observed in observed_items {
                let mut envelope = observed_responses_stream_item_envelope(
                    agent_ctx,
                    &observed,
                    self.request_id,
                    &provider_for_event,
                    &model_for_event,
                    self.app_state.config().agent.policy_mode.as_str(),
                    agent_fields_for_event.alephant_agent_name.as_deref(),
                    agent_fields_for_event.alephant_agent_name_source.as_deref(),
                    agent_fields_for_event.alephant_agent_trust_level.as_deref(),
                );
                apply_auth_scope_to_observed_tool_event(
                    &mut envelope,
                    self.auth_ctx.org_id,
                    self.auth_ctx.virtual_key_id,
                );
                if let Err(err) = emit_agent_event(&self.app_state, &self.auth_ctx, &envelope).await
                {
                    tracing::warn!(
                        error = %err,
                        request_id = %self.request_id,
                        event_type = %envelope.event_type,
                        "failed to emit observed responses stream agent event"
                    );
                }
            }
        } else if should_observe_responses_agent_items(
            self.app_state.config().agent.enabled,
            self.mapper_ctx.is_stream,
            self.mapper_ctx.client_response_semantic,
            self.agent_ctx.as_ref(),
        ) && let Some(agent_ctx) = self.agent_ctx.as_ref()
        {
            let observed_items = observe_responses_nonstream_agent_items(&response_body);
            for observed in observed_items {
                let mut envelope = observed_responses_item_envelope(
                    agent_ctx,
                    &observed,
                    self.request_id,
                    &provider_for_event,
                    &model_for_event,
                    self.app_state.config().agent.policy_mode.as_str(),
                    agent_fields_for_event.alephant_agent_name.as_deref(),
                    agent_fields_for_event.alephant_agent_name_source.as_deref(),
                    agent_fields_for_event.alephant_agent_trust_level.as_deref(),
                );
                apply_auth_scope_to_observed_tool_event(
                    &mut envelope,
                    self.auth_ctx.org_id,
                    self.auth_ctx.virtual_key_id,
                );
                if let Err(err) = emit_agent_event(&self.app_state, &self.auth_ctx, &envelope).await
                {
                    tracing::warn!(
                        error = %err,
                        request_id = %self.request_id,
                        event_type = %envelope.event_type,
                        "failed to emit observed responses agent event"
                    );
                }
            }
        }
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
            extensions::{
                ClientResponseSemantic, LoggerResponseWireSemantic, PromptCompressionTokenPair,
            },
            usage_tokens::UsageTokenCounts,
        },
    };

    #[test]
    fn extract_request_properties_includes_session_properties() {
        let mut headers = HeaderMap::new();
        headers.insert("alephant-property-custom", HeaderValue::from_static("keep"));
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

    #[test]
    fn responses_observer_requires_responses_semantic() {
        let ctx = crate::agent::context::AgentContext {
            agent_id_external: Some("agent-1".to_string()),
            run_id: Some("run-1".to_string()),
            ..Default::default()
        };

        assert!(super::should_observe_responses_agent_items(
            true,
            false,
            crate::types::extensions::ClientResponseSemantic::Responses,
            Some(&ctx),
        ));
        assert!(!super::should_observe_responses_agent_items(
            true,
            false,
            crate::types::extensions::ClientResponseSemantic::ChatCompletions,
            Some(&ctx),
        ));
        assert!(!super::should_observe_responses_agent_items(
            true,
            true,
            crate::types::extensions::ClientResponseSemantic::Responses,
            Some(&ctx),
        ));
    }

    #[test]
    fn responses_observer_allows_agent_uid_only_context() {
        let ctx = crate::agent::context::AgentContext {
            agent_uid: Some(uuid::Uuid::nil()),
            ..Default::default()
        };

        assert!(super::should_observe_responses_agent_items(
            true,
            false,
            crate::types::extensions::ClientResponseSemantic::Responses,
            Some(&ctx),
        ));
    }

    #[test]
    fn should_observe_responses_stream_requires_responses_sse_wire() {
        let ctx = crate::agent::context::AgentContext {
            agent_id_external: Some("agent-1".to_string()),
            run_id: Some("run-1".to_string()),
            ..Default::default()
        };

        assert!(super::should_observe_responses_stream_agent_items(
            true,
            true,
            crate::types::extensions::ClientResponseSemantic::Responses,
            crate::types::extensions::LoggerResponseWireSemantic::ResponsesSse,
            Some(&ctx),
        ));
        assert!(!super::should_observe_responses_stream_agent_items(
            true,
            true,
            crate::types::extensions::ClientResponseSemantic::Responses,
            crate::types::extensions::LoggerResponseWireSemantic::ChatCompletionsSse,
            Some(&ctx),
        ));
        assert!(!super::should_observe_responses_stream_agent_items(
            true,
            false,
            crate::types::extensions::ClientResponseSemantic::Responses,
            crate::types::extensions::LoggerResponseWireSemantic::ResponsesSse,
            Some(&ctx),
        ));
        assert!(!super::should_observe_responses_stream_agent_items(
            false,
            true,
            crate::types::extensions::ClientResponseSemantic::Responses,
            crate::types::extensions::LoggerResponseWireSemantic::ResponsesSse,
            Some(&ctx),
        ));
        assert!(!super::should_observe_responses_stream_agent_items(
            true,
            true,
            crate::types::extensions::ClientResponseSemantic::ChatCompletions,
            crate::types::extensions::LoggerResponseWireSemantic::ResponsesSse,
            Some(&ctx),
        ));
    }

    #[test]
    fn responses_stream_observer_ignores_chat_completions_sse_even_with_agent_ctx() {
        let ctx = crate::agent::context::AgentContext {
            agent_id_external: Some("agent-1".to_string()),
            run_id: Some("run-1".to_string()),
            ..Default::default()
        };

        assert!(!super::should_observe_responses_stream_agent_items(
            true,
            true,
            ClientResponseSemantic::ChatCompletions,
            LoggerResponseWireSemantic::ChatCompletionsSse,
            Some(&ctx),
        ));
    }

    #[test]
    fn responses_stream_observer_ignores_responses_json_non_stream() {
        let ctx = crate::agent::context::AgentContext {
            agent_id_external: Some("agent-1".to_string()),
            run_id: Some("run-1".to_string()),
            ..Default::default()
        };

        assert!(!super::should_observe_responses_stream_agent_items(
            true,
            false,
            ClientResponseSemantic::Responses,
            LoggerResponseWireSemantic::ResponsesJson,
            Some(&ctx),
        ));
    }

    #[test]
    fn responses_stream_observer_ignores_cursor_codex_responses_chat_bridge() {
        let ctx = crate::agent::context::AgentContext {
            agent_id_external: Some("agent-1".to_string()),
            run_id: Some("run-1".to_string()),
            ..Default::default()
        };

        assert!(!super::should_observe_responses_stream_agent_items(
            true,
            true,
            ClientResponseSemantic::Responses,
            LoggerResponseWireSemantic::ChatCompletionsSse,
            Some(&ctx),
        ));
    }

    #[test]
    fn observed_responses_stream_envelope_derives_step_id_and_parent() {
        let ctx = crate::agent::context::AgentContext {
            agent_id_external: Some("agent-1".to_string()),
            run_id: Some("run-1".to_string()),
            step_id: Some("parent-step-1".to_string()),
            ..Default::default()
        };
        let observed = crate::agent::tool_observer::ObservedResponsesStreamItem {
            kind: crate::agent::tool_observer::ObservedResponsesStreamItemKind::FunctionCall,
            event_type: "tool.call.observed",
            response_id: Some("resp_1".to_string()),
            sequence: 7,
            output_index: Some(2),
            item_id: Some("fc_1".to_string()),
            call_id: Some("call_1".to_string()),
            name: Some("lookup_price".to_string()),
            status: Some("completed".to_string()),
            idempotency_key: "responses_stream:resp_1:2:fc_1".to_string(),
            metadata: serde_json::json!({
                "observer": "responses_stream_observer",
                "source_event_type": "response.output_item.done"
            }),
        };

        let envelope = super::observed_responses_stream_item_envelope(
            &ctx,
            &observed,
            uuid::Uuid::nil(),
            "openai",
            "gpt-4.1",
            "monitor",
            Some("Test Agent"),
            Some("registered"),
            Some("self_reported"),
        );

        assert_eq!(
            envelope.step_id.as_deref(),
            Some(
                "gwobs:responses:00000000-0000-0000-0000-000000000000:resp_1:\
                 2:fc_1:tool_call_observed"
            )
        );
        assert_eq!(envelope.parent_step_id.as_deref(), Some("parent-step-1"));
        assert_eq!(envelope.sequence, Some(7));
        assert_eq!(envelope.run_id.as_deref(), Some("run-1"));
        assert_eq!(envelope.tool_call_id.as_deref(), Some("call_1"));
        assert_eq!(
            envelope.step_kind,
            Some(crate::agent::context::AgentStepKind::ToolCall)
        );
        assert_eq!(
            envelope.metadata["observer"].as_str(),
            Some("responses_stream_observer")
        );
        assert_eq!(
            envelope.metadata["source_event_type"].as_str(),
            Some("response.output_item.done")
        );
        assert_eq!(envelope.metadata["provider"].as_str(), Some("openai"));
        assert_eq!(envelope.metadata["model"].as_str(), Some("gpt-4.1"));
        assert_eq!(envelope.metadata["response_id"].as_str(), Some("resp_1"));
        assert_eq!(envelope.metadata["output_index"].as_u64(), Some(2));
        assert_eq!(envelope.metadata["item_id"].as_str(), Some("fc_1"));
        assert_eq!(envelope.metadata["call_id"].as_str(), Some("call_1"));
    }

    #[test]
    fn responses_stream_observer_path_builds_observed_events_after_log_gating() {
        let request_id =
            uuid::Uuid::parse_str("01890f5a-52fd-7b9a-b51e-33a22f7b6f24").expect("uuid");
        let ctx = crate::agent::context::AgentContext {
            agent_id_external: Some("agent-1".to_string()),
            run_id: Some("run-1".to_string()),
            step_id: Some("parent-step-1".to_string()),
            ..Default::default()
        };
        let body = bytes::Bytes::from_static(
            br#"data: {"type":"response.output_item.done","response_id":"resp_1","output_index":0,"ignored_secret":"ALEPHANT_SECRET_TOKEN","item":{"id":"fc_1","type":"function_call","call_id":"call_123","name":"lookup_price","arguments":"{\"symbol\":\"AAPL\"}","status":"completed"}}

data: [DONE]

"#,
        );

        assert!(super::should_observe_responses_stream_agent_items(
            true,
            true,
            crate::types::extensions::ClientResponseSemantic::Responses,
            crate::types::extensions::LoggerResponseWireSemantic::ResponsesSse,
            Some(&ctx),
        ));

        let observed_items = super::observe_responses_stream_agent_items(request_id, &body);
        assert_eq!(observed_items.len(), 1);

        let mut envelope = super::observed_responses_stream_item_envelope(
            &ctx,
            &observed_items[0],
            request_id,
            "openai",
            "gpt-4.1",
            "monitor",
            Some("Test Agent"),
            Some("registered"),
            Some("auth_bound"),
        );
        let workspace_id = uuid::Uuid::new_v4();
        let virtual_key_id = uuid::Uuid::new_v4();
        super::apply_auth_scope_to_observed_tool_event(
            &mut envelope,
            workspace_id,
            Some(virtual_key_id),
        );

        assert_eq!(envelope.event_type, "tool.call.observed");
        assert_eq!(envelope.tool_call_id.as_deref(), Some("call_123"));
        assert_eq!(envelope.workspace_id, workspace_id.to_string());
        assert_eq!(envelope.virtual_key_id, Some(virtual_key_id));
        assert!(
            !envelope
                .metadata
                .to_string()
                .contains("ALEPHANT_SECRET_TOKEN")
        );
    }

    #[test]
    fn should_observe_chat_completion_stream_requires_chat_sse_and_agent_context() {
        let ctx = crate::agent::context::AgentContext {
            agent_id_external: Some("agent-1".to_string()),
            run_id: Some("run-1".to_string()),
            ..Default::default()
        };

        assert!(super::should_observe_chat_completion_stream_tool_calls(
            true,
            true,
            ClientResponseSemantic::ChatCompletions,
            LoggerResponseWireSemantic::ChatCompletionsSse,
            false,
            false,
            false,
            Some(&ctx),
        ));
        assert!(!super::should_observe_chat_completion_stream_tool_calls(
            false,
            true,
            ClientResponseSemantic::ChatCompletions,
            LoggerResponseWireSemantic::ChatCompletionsSse,
            false,
            false,
            false,
            Some(&ctx),
        ));
        assert!(!super::should_observe_chat_completion_stream_tool_calls(
            true,
            false,
            ClientResponseSemantic::ChatCompletions,
            LoggerResponseWireSemantic::ChatCompletionsSse,
            false,
            false,
            false,
            Some(&ctx),
        ));
        assert!(!super::should_observe_chat_completion_stream_tool_calls(
            true,
            true,
            ClientResponseSemantic::Responses,
            LoggerResponseWireSemantic::ChatCompletionsSse,
            false,
            false,
            false,
            Some(&ctx),
        ));
        assert!(!super::should_observe_chat_completion_stream_tool_calls(
            true,
            true,
            ClientResponseSemantic::ChatCompletions,
            LoggerResponseWireSemantic::ResponsesSse,
            false,
            false,
            false,
            Some(&ctx),
        ));
        assert!(!super::should_observe_chat_completion_stream_tool_calls(
            true,
            true,
            ClientResponseSemantic::ChatCompletions,
            LoggerResponseWireSemantic::ChatCompletionsSse,
            false,
            false,
            false,
            None,
        ));

        let empty = crate::agent::context::AgentContext::default();
        assert!(!super::should_observe_chat_completion_stream_tool_calls(
            true,
            true,
            ClientResponseSemantic::ChatCompletions,
            LoggerResponseWireSemantic::ChatCompletionsSse,
            false,
            false,
            false,
            Some(&empty),
        ));
    }

    #[test]
    fn chat_completion_stream_observer_ignores_responses_bridge_paths() {
        let ctx = crate::agent::context::AgentContext {
            agent_id_external: Some("agent-1".to_string()),
            run_id: Some("run-1".to_string()),
            ..Default::default()
        };

        for (
            unified_responses_bridge_chat_completions_sse,
            cursor_responses_via_chat_completions,
            client_expects_responses_wire,
        ) in [
            (true, false, false),
            (false, true, false),
            (false, false, true),
        ] {
            assert!(!super::should_observe_chat_completion_stream_tool_calls(
                true,
                true,
                ClientResponseSemantic::ChatCompletions,
                LoggerResponseWireSemantic::ChatCompletionsSse,
                unified_responses_bridge_chat_completions_sse,
                cursor_responses_via_chat_completions,
                client_expects_responses_wire,
                Some(&ctx),
            ));
        }
    }

    #[test]
    fn observed_chat_stream_envelope_uses_gateway_child_step_and_parent() {
        let request_id =
            uuid::Uuid::parse_str("01890f5a-52fd-7b9a-b51e-33a22f7b6f24").expect("uuid");
        let agent_uid =
            uuid::Uuid::parse_str("01890f5a-52fd-7b9a-b51e-33a22f7b6f25").expect("uuid");
        let ctx = crate::agent::context::AgentContext {
            agent_id_external: Some("support-bot".to_string()),
            agent_uid: Some(agent_uid),
            run_id: Some("run-1".to_string()),
            step_id: Some("parent-step-1".to_string()),
            handoff_id: Some("handoff-1".to_string()),
            graph_node: Some("planner".to_string()),
            trust_level: crate::agent::context::AgentTrustLevel::SelfReported,
            ..Default::default()
        };
        let observed = crate::agent::tool_observer::ObservedChatCompletionStreamToolCall {
            choice_index: 3,
            tool_call_index: Some(2),
            tool_call_id: Some("call_abc".to_string()),
            tool_name: Some("lookup_price".to_string()),
            tool_type: Some("function".to_string()),
            arguments_summary: Some("{\"symbol\":\"AAPL\"}".to_string()),
            arguments_hash: Some("sha256:abc".to_string()),
            arguments_chars: Some(17),
            arguments_valid_json: Some(true),
            arguments_truncated: false,
            chunk_count: 4,
            finish_reason: Some("tool_calls".to_string()),
            sse_done_seen: true,
            aggregation_status: crate::agent::tool_observer::ChatStreamAggregationStatus::Complete,
            idempotency_key: "chatcmpl:obs".to_string(),
            metadata: serde_json::json!({
                "observer": "chat_completions_stream_tool_observer",
                "aggregation_status": "complete",
                "choice_index": 3
            }),
        };

        let envelope = super::observed_chat_stream_tool_call_envelope(
            &ctx,
            &observed,
            request_id,
            "openai",
            "gpt-4.1",
            "monitor",
            Some("Registered Bot"),
            Some("virtual_key_label"),
            Some("auth_bound"),
        );

        assert_eq!(envelope.event_type, "tool.call.observed");
        assert_eq!(
            envelope.step_id.as_deref(),
            Some(
                "gwobs:chatcmpl:01890f5a-52fd-7b9a-b51e-33a22f7b6f24:3:\
                 index_2:tool_call_observed"
            )
        );
        assert_eq!(envelope.parent_step_id.as_deref(), Some("parent-step-1"));
        assert_eq!(envelope.tool_call_id.as_deref(), Some("call_abc"));
        assert_eq!(envelope.name.as_deref(), Some("lookup_price"));
        assert_eq!(
            envelope.step_kind,
            Some(crate::agent::context::AgentStepKind::ToolCall)
        );
        assert_eq!(
            envelope.step_source,
            crate::agent::context::AgentStepSource::Gateway
        );
        assert_eq!(
            envelope.step_confidence,
            crate::agent::context::AgentConfidence::Medium
        );
        assert_eq!(
            envelope.event_source_trust,
            crate::agent::context::AgentEventSourceTrust::GatewayObserved
        );
        assert_eq!(envelope.agent_id_external.as_deref(), Some("support-bot"));
        assert_eq!(envelope.agent_uid, Some(agent_uid));
        assert_eq!(envelope.run_id.as_deref(), Some("run-1"));
        assert_eq!(envelope.handoff_id.as_deref(), Some("handoff-1"));
        assert_eq!(envelope.graph_node.as_deref(), Some("planner"));
        assert_eq!(
            envelope.trust_level,
            crate::agent::context::AgentTrustLevel::SelfReported
        );
        assert_eq!(
            envelope.alephant_agent_name.as_deref(),
            Some("Registered Bot")
        );
        assert_eq!(
            envelope.alephant_agent_name_source.as_deref(),
            Some("virtual_key_label")
        );
        assert_eq!(
            envelope.alephant_agent_trust_level.as_deref(),
            Some("auth_bound")
        );
        assert_eq!(
            envelope.metadata["observer"].as_str(),
            Some("chat_completions_stream_tool_observer")
        );
        assert_eq!(envelope.metadata["provider"].as_str(), Some("openai"));
        assert_eq!(envelope.metadata["model"].as_str(), Some("gpt-4.1"));
        assert_eq!(
            envelope.metadata["request_id"].as_str(),
            Some("01890f5a-52fd-7b9a-b51e-33a22f7b6f24")
        );
        assert_eq!(envelope.metadata["choice_index"].as_u64(), Some(3));
    }

    #[test]
    fn chat_stream_observer_path_builds_observed_events_after_log_gating() {
        let request_id =
            uuid::Uuid::parse_str("01890f5a-52fd-7b9a-b51e-33a22f7b6f24").expect("uuid");
        let ctx = crate::agent::context::AgentContext {
            agent_id_external: Some("support-bot".to_string()),
            run_id: Some("run-1".to_string()),
            step_id: Some("parent-step-1".to_string()),
            ..Default::default()
        };
        let body = concat!(
            "data: {\"id\":\"chatcmpl_1\",\"choices\":[{\"index\":0,\"delta\":\
             {\"tool_calls\":[{\"index\":0,\"id\":\"call_123\",\"type\":\"\
             function\",\"function\":{\"name\":\"lookup_price\",\"arguments\":\
             \"\"}}]}}]}\n\n",
            "data: {\"id\":\"chatcmpl_1\",\"choices\":[{\"index\":0,\"delta\":\
             {\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"\
             \"}}]}}]}\n\n",
            "data: {\"id\":\"chatcmpl_1\",\"choices\":[{\"index\":0,\"delta\":\
             {\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\
             symbol\"}}]}}]}\n\n",
            "data: {\"id\":\"chatcmpl_1\",\"choices\":[{\"index\":0,\"delta\":\
             {\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\":\
             \\\"AAPL\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        );

        assert!(super::should_observe_chat_completion_stream_tool_calls(
            true,
            true,
            ClientResponseSemantic::ChatCompletions,
            LoggerResponseWireSemantic::ChatCompletionsSse,
            false,
            false,
            false,
            Some(&ctx),
        ));

        let observed =
            super::observe_chat_completion_stream_tool_calls(request_id, body.as_bytes());
        assert_eq!(observed.len(), 1);

        let mut envelope = super::observed_chat_stream_tool_call_envelope(
            &ctx,
            &observed[0],
            request_id,
            "openai",
            "gpt-4.1",
            "monitor",
            Some("Support Bot"),
            Some("virtual_key_label"),
            Some("auth_bound"),
        );
        super::apply_auth_scope_to_observed_tool_event(&mut envelope, uuid::Uuid::nil(), None);

        assert_eq!(
            envelope.metadata["observer"].as_str(),
            Some("chat_completions_stream_tool_observer")
        );
        assert_eq!(
            envelope.metadata["source_wire"].as_str(),
            Some("chat_completions_sse")
        );
        assert_eq!(envelope.parent_step_id.as_deref(), Some("parent-step-1"));
        assert_eq!(envelope.tool_call_id.as_deref(), Some("call_123"));
        assert_eq!(envelope.name.as_deref(), Some("lookup_price"));
    }

    #[test]
    fn observed_responses_reasoning_envelope_uses_reasoning_step_kind() {
        let ctx = crate::agent::context::AgentContext {
            agent_id_external: Some("agent-1".to_string()),
            run_id: Some("run-1".to_string()),
            ..Default::default()
        };
        let observed = crate::agent::tool_observer::ObservedResponsesItem {
            kind: crate::agent::tool_observer::ObservedResponsesItemKind::Reasoning,
            event_type: "llm.reasoning.observed",
            response_id: Some("resp_1".to_string()),
            output_index: Some(0),
            item_id: Some("rs_1".to_string()),
            call_id: None,
            name: Some("Reasoning summary observed".to_string()),
            status: None,
            metadata: serde_json::json!({
                "observer": "responses_nonstream_agent_observer",
                "summary_count": 1
            }),
        };

        let envelope = super::observed_responses_item_envelope(
            &ctx,
            &observed,
            uuid::Uuid::nil(),
            "openai",
            "gpt-4.1",
            "monitor",
            Some("Test Agent"),
            Some("registered"),
            Some("self_reported"),
        );

        assert_eq!(envelope.event_type, "llm.reasoning.observed");
        assert_eq!(
            envelope.step_kind,
            Some(crate::agent::context::AgentStepKind::Reasoning)
        );
        assert_eq!(envelope.name.as_deref(), Some("Reasoning summary observed"));
        assert_eq!(
            envelope.metadata["observer"].as_str(),
            Some("responses_nonstream_agent_observer")
        );
    }
}

#[cfg(test)]
mod agent_property_tests {
    use indexmap::IndexMap;
    use uuid::Uuid;

    use crate::agent::context::{
        AgentConfidence, AgentContext, AgentStepKind, AgentStepSource, AgentTrustLevel,
    };

    #[test]
    fn agent_log_fields_extracts_standard_keys_without_properties() {
        let mut properties = IndexMap::new();
        properties.insert("alephant-property-custom".to_string(), "keep".to_string());
        let agent_uid =
            Uuid::parse_str("01890f5a-52fd-7b9a-b51e-33a22f7b6f24").expect("static uuid is valid");
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

        super::apply_final_agent_name_to_request_log(None, &mut fields, &mut properties);

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

        super::apply_final_agent_name_to_request_log(None, &mut fields, &mut properties);

        assert_eq!(fields.alephant_agent_name, None);
        assert_eq!(fields.alephant_agent_name_source, None);
        assert_eq!(fields.alephant_agent_trust_level, None);
        assert!(properties.is_empty());
    }

    #[test]
    fn observed_tool_call_envelope_inherits_agent_context() {
        let request_id = Uuid::parse_str("01890f5a-52fd-7b9a-b51e-33a22f7b6f24").expect("uuid");
        let ctx = AgentContext {
            agent_id_external: Some("support-bot".to_string()),
            agent_name: Some("Support Bot".to_string()),
            run_id: Some("run-1".to_string()),
            step_id: Some("step-2".to_string()),
            graph_node: Some("tool-planner".to_string()),
            trust_level: AgentTrustLevel::SelfReported,
            ..AgentContext::default()
        };
        let observed = crate::agent::tool_observer::ObservedToolCall {
            tool_call_id: Some("call-1".to_string()),
            tool_name: "zendesk.get_ticket".to_string(),
            tool_type: Some("function".to_string()),
            arguments_summary: Some("{\"ticket_id\":\"T-1\"}".to_string()),
            choice_index: Some(0),
        };

        let envelope = super::observed_tool_call_envelope(
            &ctx,
            &observed,
            request_id,
            "openai",
            "gpt-4o",
            "audit",
            Some("Registered Bot"),
            Some("virtual_key_label"),
            Some("auth_bound"),
        );

        assert_eq!(envelope.event_type, "tool.call.observed");
        assert_eq!(envelope.agent_id_external.as_deref(), Some("support-bot"));
        assert_eq!(envelope.run_id.as_deref(), Some("run-1"));
        assert_eq!(envelope.step_id.as_deref(), Some("step-2"));
        assert_eq!(envelope.tool_call_id.as_deref(), Some("call-1"));
        assert_eq!(envelope.name.as_deref(), Some("zendesk.get_ticket"));
        assert_eq!(envelope.step_kind, Some(AgentStepKind::ToolCall));
        assert_eq!(
            envelope.event_source_trust,
            crate::agent::context::AgentEventSourceTrust::GatewayObserved
        );
        assert_eq!(
            envelope.metadata["observer"],
            "chat_completions_tool_observer"
        );
        assert_eq!(envelope.metadata["provider"], "openai");
        assert_eq!(envelope.metadata["model"], "gpt-4o");
        assert_eq!(envelope.metadata["request_id"], request_id.to_string());
        assert_eq!(envelope.metadata["tool_type"], "function");
        assert_eq!(
            envelope.metadata["arguments_summary"],
            "{\"ticket_id\":\"T-1\"}"
        );
        assert_eq!(
            envelope.alephant_agent_name.as_deref(),
            Some("Registered Bot")
        );
    }

    #[test]
    fn should_observe_tool_calls_requires_agent_enabled_context_and_non_stream() {
        let ctx = AgentContext {
            run_id: Some("run-1".to_string()),
            ..AgentContext::default()
        };

        assert!(super::should_observe_chat_completion_tool_calls(
            true,
            false,
            crate::types::extensions::ClientResponseSemantic::ChatCompletions,
            Some(&ctx),
        ));
        assert!(!super::should_observe_chat_completion_tool_calls(
            false,
            false,
            crate::types::extensions::ClientResponseSemantic::ChatCompletions,
            Some(&ctx),
        ));
        assert!(!super::should_observe_chat_completion_tool_calls(
            true,
            true,
            crate::types::extensions::ClientResponseSemantic::ChatCompletions,
            Some(&ctx),
        ));
        assert!(!super::should_observe_chat_completion_tool_calls(
            true,
            false,
            crate::types::extensions::ClientResponseSemantic::ChatCompletions,
            None,
        ));
        assert!(!super::should_observe_chat_completion_tool_calls(
            true,
            false,
            crate::types::extensions::ClientResponseSemantic::Responses,
            Some(&ctx),
        ));

        let empty = AgentContext::default();
        assert!(!super::should_observe_chat_completion_tool_calls(
            true,
            false,
            crate::types::extensions::ClientResponseSemantic::ChatCompletions,
            Some(&empty),
        ));
    }

    #[test]
    fn observed_tool_call_envelope_fills_auth_scoped_fields_before_send() {
        let request_id = Uuid::new_v4();
        let workspace_id = Uuid::new_v4();
        let virtual_key_id = Uuid::new_v4();
        let ctx = AgentContext {
            run_id: Some("run-1".to_string()),
            ..AgentContext::default()
        };
        let observed = crate::agent::tool_observer::ObservedToolCall {
            tool_call_id: Some("call-1".to_string()),
            tool_name: "search".to_string(),
            tool_type: Some("function".to_string()),
            arguments_summary: None,
            choice_index: Some(0),
        };

        let mut envelope = super::observed_tool_call_envelope(
            &ctx, &observed, request_id, "openai", "gpt-4o", "audit", None, None, None,
        );

        super::apply_auth_scope_to_observed_tool_event(
            &mut envelope,
            workspace_id,
            Some(virtual_key_id),
        );

        assert_eq!(envelope.workspace_id, workspace_id.to_string());
        assert_eq!(envelope.virtual_key_id, Some(virtual_key_id));
    }
}
