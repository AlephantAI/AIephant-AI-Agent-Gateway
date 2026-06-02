use chrono::{DateTime, Utc};
use http::HeaderMap;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use typed_builder::TypedBuilder;
use url::Url;
use uuid::Uuid;

use super::{org::OrgId, user::UserId};
use crate::{
    config::deployment_target::DeploymentTarget,
    error::logger::LoggerError,
    types::{
        extensions::{LargeContextDecision, PromptContext},
        router::RouterId,
    },
};

/// Values for `alephantMeta.aiGatewayBodyMapping` (logs-collector
/// `BodyMappingType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AiGatewayBodyMapping {
    Openai,
    NoMapping,
    Responses,
}

#[inline]
fn uuid_is_nil(id: &Uuid) -> bool {
    *id == Uuid::nil()
}

#[inline]
fn log_entity_type_skip(s: &str) -> bool {
    s != "member" && s != "agent"
}

#[inline]
fn log_entity_name_skip(s: &str) -> bool {
    s.is_empty()
}

/// Omit `cacheReferenceId` when unset, empty, or the all-zero UUID placeholder.
/// Legacy paths once sent `Uuid::nil()` when a response id header was missing;
/// that is not a valid cache key and confuses downstream.
#[inline]
#[allow(clippy::ref_option)] // serde `skip_serializing_if` passes `&Option<T>`
fn skip_cache_reference_id_sentinel(v: &Option<String>) -> bool {
    match v {
        None => true,
        Some(s) => {
            let t = s.trim();
            t.is_empty()
                || t.eq_ignore_ascii_case(
                    "00000000-0000-0000-0000-000000000000",
                )
        }
    }
}

#[inline]
fn default_request_log_storage_location() -> String {
    "clickhouse".to_string()
}

#[derive(Debug, Serialize, Deserialize)]
pub struct S3Log {
    pub request: String,
    pub response: String,
}

impl S3Log {
    #[must_use]
    pub fn new(request: String, response: String) -> Self {
        Self { request, response }
    }
}

#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AlephantLogMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_override: Option<String>,
    pub omit_request_log: bool,
    pub omit_response_log: bool,
    pub webhook_enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub posthog_api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub posthog_host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lytix_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gateway_router_id: Option<RouterId>,
    pub gateway_deployment_target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_version_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_inputs:
        Option<std::collections::HashMap<String, serde_json::Value>>,
    /// Registry / resolved model for ingest (`alephantMeta.gatewayModel`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gateway_model: Option<String>,
    /// Subset of logs-collector `ModelProviderName`; omitted when unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gateway_provider: Option<String>,
    /// Upstream-facing model id when distinct from `gateway_model`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_model_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub large_context_handler: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub large_context_action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub large_context_original_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub large_context_effective_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub large_context_estimated_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub large_context_input_budget_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_passthrough_billing: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ai_gateway_body_mapping: Option<AiGatewayBodyMapping>,
}

impl AlephantLogMetadata {
    pub fn from_headers(
        headers: &mut HeaderMap,
        router_id: Option<RouterId>,
        deployment_target: &DeploymentTarget,
        prompt_ctx: Option<PromptContext>,
    ) -> Result<Self, LoggerError> {
        let model_override = headers
            .remove("x-alephant-model-override")
            .map(|v| v.to_str().map(std::borrow::ToOwned::to_owned))
            .transpose()?;
        let omit_request_log = headers.get("alephant-omit-request").is_some();
        let omit_response_log = headers.get("alephant-omit-response").is_some();
        let webhook_enabled =
            headers.remove("x-alephant-webhook-enabled").is_some();
        let posthog_api_key = headers
            .remove("x-alephant-posthog-api-key")
            .map(|v| v.to_str().map(std::borrow::ToOwned::to_owned))
            .transpose()?;
        let posthog_host = headers
            .remove("x-alephant-posthog-host")
            .map(|v| v.to_str().map(std::borrow::ToOwned::to_owned))
            .transpose()?;
        let lytix_key = headers
            .remove("x-alephant-lytix-key")
            .map(|v| v.to_str().map(std::borrow::ToOwned::to_owned))
            .transpose()?;
        let (prompt_id, prompt_version_id, prompt_inputs) =
            if let Some(ctx) = prompt_ctx {
                (Some(ctx.prompt_id), ctx.prompt_version_id, ctx.inputs)
            } else {
                (None, None, None)
            };

        Ok(Self {
            model_override,
            omit_request_log,
            omit_response_log,
            webhook_enabled,
            posthog_api_key,
            posthog_host,
            lytix_key,
            gateway_router_id: router_id,
            gateway_deployment_target: deployment_target
                .log_label()
                .to_string(),
            prompt_id,
            prompt_version_id,
            prompt_inputs,
            gateway_model: None,
            gateway_provider: None,
            provider_model_id: None,
            large_context_handler: None,
            large_context_action: None,
            large_context_original_model: None,
            large_context_effective_model: None,
            large_context_estimated_tokens: None,
            large_context_input_budget_tokens: None,
            is_passthrough_billing: None,
            ai_gateway_body_mapping: None,
        })
    }

    pub fn apply_large_context_decision(
        &mut self,
        decision: &LargeContextDecision,
    ) {
        self.large_context_handler =
            Some(decision.handler.as_str().to_string());
        self.large_context_action = Some(decision.action.as_str().to_string());
        self.large_context_original_model = decision.original_model.clone();
        self.large_context_effective_model = decision.effective_model.clone();
        self.large_context_estimated_tokens = decision.estimated_input_tokens;
        self.large_context_input_budget_tokens = decision.input_budget_tokens;
    }
}

#[derive(Debug, Serialize, Deserialize, TypedBuilder)]
#[serde(rename_all = "camelCase")]
pub struct RequestLog {
    pub id: Uuid,
    pub user_id: UserId,
    /// Tenant / workspace UUID for logs-collector (JSON **`workspaceId`**).
    /// Populated from [`crate::types::extensions::AuthContext::org_id`] in
    /// `LoggerService`. **`organizationId`** is accepted on deserialize for
    /// backward compatibility with older payloads.
    #[serde(alias = "organizationId")]
    pub workspace_id: OrgId,
    /// `workspaces.type::text` from PG for request-log segmentation.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub workspace_type: Option<String>,
    /// Non-empty `alephant-session-id` (see `session_headers` module).
    /// Same string as `properties["Alephant-Session-Id"]` when present; JSON
    /// **`sessionId`**.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub prompt_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub prompt_version: Option<String>,
    #[builder(default)]
    pub properties: IndexMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub alephant_agent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub alephant_agent_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub alephant_agent_name_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub alephant_agent_uid: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub alephant_run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub alephant_step_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub alephant_parent_step_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub alephant_tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub alephant_handoff_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub alephant_graph_node: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub alephant_iteration: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub alephant_state_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub alephant_step_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub alephant_step_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub alephant_step_confidence: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub alephant_agent_trust_level: Option<String>,
    /// JSON `alephantLegacyApiKeyId` (logs-collector `KafkaMessageContents`).
    #[serde(
        rename = "alephantLegacyApiKeyId",
        alias = "alephantApiKeyId",
        skip_serializing_if = "Option::is_none"
    )]
    #[builder(default)]
    pub alephant_api_key_id: Option<f64>,
    /// Maps to RMT `proxy_key_id` (JSON `alephantVirtualKeyId`).
    #[serde(
        rename = "alephantVirtualKeyId",
        skip_serializing_if = "Option::is_none"
    )]
    #[builder(default)]
    pub alephant_virtual_key_id: Option<Uuid>,
    /// Master key ID (Alephant BYO-KEY); maps to RMT `master_key_id` when the
    /// gateway sends it (JSON `alephantMasterKeyId`).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub alephant_master_key_id: Option<Uuid>,
    /// Virtual key display name → RMT `proxy_key_name` (JSON
    /// `alephantVirtualKeyName`).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub alephant_virtual_key_name: Option<String>,
    /// Virtual key prefix for display → RMT `proxy_key_prefix` (JSON
    /// `alephantVirtualKeyPrefix`).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub alephant_virtual_key_prefix: Option<String>,
    /// Department display name (denormalized) → RMT `department_name` (JSON
    /// `alephantDepartmentName`).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub alephant_department_name: Option<String>,
    /// Department UUID → RMT `department_id` (JSON `alephantDepartmentId`);
    /// omit or all-zero UUID skips serialization.
    #[serde(
        rename = "alephantDepartmentId",
        default,
        skip_serializing_if = "uuid_is_nil"
    )]
    #[builder(default_code = "Uuid::nil()")]
    pub department_id: Uuid,
    #[serde(default, skip_serializing_if = "log_entity_type_skip")]
    #[builder(default)]
    pub entity_type: String,
    #[serde(default, skip_serializing_if = "uuid_is_nil")]
    #[builder(default_code = "Uuid::nil()")]
    pub entity_id: Uuid,
    #[serde(default, skip_serializing_if = "log_entity_name_skip")]
    #[builder(default)]
    pub entity_name: String,
    pub target_url: Url,
    pub provider: String,
    /// Resolved model lives in `alephantMeta.gatewayModel`, not in
    /// `log.request`.
    #[serde(skip)]
    #[builder(default)]
    pub model: String,
    /// Request body length in bytes (JSON `bodySize`; collector schema).
    pub body_size: f64,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub threat: Option<bool>,
    /// ISO 3166-1 alpha-2 or provider-specific code string.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub country_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub request_referrer: Option<String>,
    pub request_created_at: DateTime<Utc>,
    pub is_stream: bool,
    /// Prompt template with inputs (JSON `alephantTemplate`); shape matches
    /// collector / TS `TemplateWithInputs`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub alephant_template: Option<serde_json::Value>,
    #[serde(skip)]
    #[builder(default)]
    pub assets: Vec<String>,
    #[serde(skip)]
    #[builder(default)]
    pub scores: IndexMap<String, i64>,
    /// JSON key `body`: UTF-8 text when inlined. If **either** request or
    /// response body is at least 1 MiB, **both** sides use presigned GET URL
    /// strings; if both are below 1 MiB, both are inlined.
    #[serde(rename = "body", alias = "requestBody")]
    #[builder(default)]
    pub request_body: String,
    #[builder(default = 90)]
    pub body_ttl_days: u16,
    /// Inline vs object-store bodies for this row: **`clickhouse`** when both
    /// sides are small enough to inline in JSON; **`s3`** when at least one
    /// side uses a presigned URL. JSON **`storageLocation`** (camelCase) for
    /// logs-collector / downstream; missing key on deserialize defaults to
    /// **`clickhouse`**.
    #[serde(default = "default_request_log_storage_location")]
    #[builder(default_code = r#""clickhouse".to_string()"#)]
    pub storage_location: String,
    /// Use `alephantMeta.aiGatewayBodyMapping` in JSON.
    #[serde(skip)]
    #[builder(default)]
    pub ai_gateway_body_mapping: String,
    #[serde(skip)]
    #[builder(default)]
    pub updated_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub experiment_column_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub experiment_row_index: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub cache_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub cache_seed: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub cache_bucket_max_size: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub cache_control: Option<String>,
    /// Present on cache **Fresh** hits: the Redis/cache entry key (gateway
    /// uses a stringified hash, not a UUID). Downstream may treat presence
    /// as "served from cache".
    #[serde(skip_serializing_if = "skip_cache_reference_id_sentinel")]
    #[builder(default)]
    pub cache_reference_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, TypedBuilder)]
#[serde(rename_all = "camelCase")]
pub struct ResponseLog {
    pub id: Uuid,
    pub status: i64,
    pub body_size: f64,
    /// End-to-end latency in milliseconds (JSON `delayMs`).
    #[serde(rename = "delayMs", alias = "latency")]
    pub latency: i64,
    /// Time to first token in milliseconds.
    pub time_to_first_token: i64,
    pub response_created_at: DateTime<Utc>,
    /// JSON key `body`; same inline / presigned rules as
    /// [`RequestLog::request_body`].
    #[serde(rename = "body", alias = "responseBody")]
    #[builder(default)]
    pub response_body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    pub model: Option<String>,
    /// Pre-compression input token estimate when compression ran; otherwise
    /// matches [`Self::prompt_tokens`].
    #[builder(default)]
    pub origin_prompt_tokens: i64,
    #[builder(default)]
    pub prompt_tokens: i64,
    #[builder(default)]
    pub completion_tokens: i64,
    #[builder(default)]
    pub prompt_cache_write_tokens: i64,
    #[builder(default)]
    pub prompt_cache_read_tokens: i64,
    #[builder(default)]
    pub prompt_audio_tokens: i64,
    #[builder(default)]
    pub completion_audio_tokens: i64,
    #[builder(default)]
    pub reasoning_tokens: i64,
    /// Gateway no longer prices responses here; the downstream logging /
    /// billing path owns cost calculation, so this field is emitted as `0.0`.
    #[builder(default)]
    pub cost: f64,
    /// Use `alephantMeta.isPassthroughBilling` in JSON.
    #[serde(skip)]
    #[builder(default)]
    pub is_passthrough_billing: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Log {
    pub request: RequestLog,
    pub response: ResponseLog,
}

impl Log {
    #[must_use]
    pub fn new(request: RequestLog, response: ResponseLog) -> Self {
        Self { request, response }
    }
}

#[derive(Debug, Serialize, Deserialize, TypedBuilder)]
#[serde(rename_all = "camelCase")]
pub struct LogMessage {
    pub authorization: String,
    pub alephant_meta: AlephantLogMetadata,
    pub log: Log,
    /// Not part of `KafkaMessageContents`; omitted from JSON.
    #[serde(skip)]
    #[builder(default)]
    pub gateway_endpoint_version: Option<String>,
}

#[cfg(test)]
mod tests {
    use http::HeaderValue;
    use indexmap::IndexMap;
    use url::Url;

    use super::*;
    use crate::{
        config::deployment_target::DeploymentTarget,
        middleware::large_context::headers::TokenLimitExceptionHandler,
        types::{
            extensions::{LargeContextAction, LargeContextDecision},
            org::OrgId,
            user::UserId,
        },
    };

    #[test]
    fn request_log_session_id_serializes_when_some_omits_when_none() {
        let id = Uuid::nil();
        let user_id = UserId::new(Uuid::nil());
        let target_url: Url = "https://example.com/".parse().unwrap();
        let request_created_at = Utc::now();
        let base = || {
            RequestLog::builder()
                .id(id)
                .user_id(user_id)
                .workspace_id(OrgId::new(Uuid::nil()))
                .properties(IndexMap::new())
                .target_url(target_url.clone())
                .provider("OPENAI".to_string())
                .model("gpt-4o".to_string())
                .body_size(0.0)
                .path("/v1/chat/completions".to_string())
                .request_created_at(request_created_at)
                .is_stream(false)
        };
        let with_session = base()
            .session_id(Some("preferred-session".to_string()))
            .build();
        let json = serde_json::to_string(&with_session).unwrap();
        assert!(
            json.contains("\"sessionId\":\"preferred-session\""),
            "session_id Some(_) should serialize sessionId, got {json}",
        );
        let without_session = base().build();
        let json_none = serde_json::to_string(&without_session).unwrap();
        assert!(
            !json_none.contains("sessionId"),
            "session_id None should omit sessionId key, got {json_none}",
        );
    }

    #[test]
    fn request_log_cache_reference_id_serialization() {
        let id = Uuid::nil();
        let user_id = UserId::new(Uuid::nil());
        let target_url: Url = "https://example.com/".parse().unwrap();
        let request_created_at = Utc::now();

        let with_ref = RequestLog::builder()
            .id(id)
            .user_id(user_id)
            .workspace_id(OrgId::new(Uuid::nil()))
            .properties(IndexMap::new())
            .target_url(target_url.clone())
            .provider("OPENAI".to_string())
            .model("gpt-4o".to_string())
            .body_size(0.0)
            .path("/v1/chat/completions".to_string())
            .request_created_at(request_created_at)
            .is_stream(false)
            .cache_reference_id(Some("gateway-cache-key-123".to_string()))
            .build();
        let json = serde_json::to_string(&with_ref).unwrap();
        assert!(
            json.contains("\"cacheReferenceId\":\"gateway-cache-key-123\""),
            "cache_reference_id Some(_) should serialize cache key string, \
             got {json}",
        );

        let with_none = RequestLog::builder()
            .id(id)
            .user_id(user_id)
            .workspace_id(OrgId::new(Uuid::nil()))
            .properties(IndexMap::new())
            .target_url(target_url.clone())
            .provider("OPENAI".to_string())
            .model("gpt-4o".to_string())
            .body_size(0.0)
            .path("/v1/chat/completions".to_string())
            .request_created_at(request_created_at)
            .is_stream(false)
            .build();
        let json_none = serde_json::to_string(&with_none).unwrap();
        assert!(
            !json_none.contains("cacheReferenceId"),
            "cache_reference_id None should omit cacheReferenceId key, got \
             {json_none}",
        );

        let with_nil_placeholder = RequestLog::builder()
            .id(id)
            .user_id(user_id)
            .workspace_id(OrgId::new(Uuid::nil()))
            .properties(IndexMap::new())
            .target_url(target_url)
            .provider("OPENAI".to_string())
            .model("gpt-4o".to_string())
            .body_size(0.0)
            .path("/v1/chat/completions".to_string())
            .request_created_at(request_created_at)
            .is_stream(false)
            .cache_reference_id(Some(
                "00000000-0000-0000-0000-000000000000".to_string(),
            ))
            .build();
        let json_nil = serde_json::to_string(&with_nil_placeholder).unwrap();
        assert!(
            !json_nil.contains("cacheReferenceId"),
            "all-zero UUID placeholder should omit cacheReferenceId, got \
             {json_nil}",
        );
    }

    #[test]
    fn log_message_cache_reference_id_serialization() {
        let id = Uuid::nil();
        let user_id = UserId::new(Uuid::nil());
        let target_url: Url = "https://example.com/".parse().unwrap();
        let request_created_at = Utc::now();
        let request_log = RequestLog::builder()
            .id(id)
            .user_id(user_id)
            .workspace_id(OrgId::new(Uuid::nil()))
            .properties(IndexMap::new())
            .target_url(target_url)
            .provider("OPENAI".to_string())
            .model("gpt-4o".to_string())
            .body_size(0.0)
            .path("/v1/chat/completions".to_string())
            .request_created_at(request_created_at)
            .is_stream(false)
            .cache_reference_id(Some("log-message-cache-key".to_string()))
            .build();
        let response_log = ResponseLog::builder()
            .id(id)
            .status(200)
            .body_size(0.0)
            .latency(42)
            .time_to_first_token(7)
            .response_created_at(Utc::now())
            .build();
        let log = Log::new(request_log, response_log);
        let msg = LogMessage::builder()
            .authorization("sk-test".to_string())
            .alephant_meta(AlephantLogMetadata::default())
            .log(log)
            .build();
        let json = serde_json::to_string(&msg).unwrap();
        assert!(
            json.contains("cacheReferenceId"),
            "LogMessage with cache_reference_id should contain \
             cacheReferenceId in JSON, got {json}",
        );
    }

    #[test]
    fn metadata_reads_alephant_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-alephant-model-override",
            HeaderValue::from_static("gpt-4o-mini"),
        );
        headers.insert(
            "x-alephant-webhook-enabled",
            HeaderValue::from_static("1"),
        );
        headers.insert(
            "x-alephant-posthog-api-key",
            HeaderValue::from_static("ph-key"),
        );
        headers.insert(
            "x-alephant-posthog-host",
            HeaderValue::from_static("https://ph.example"),
        );
        headers
            .insert("x-alephant-lytix-key", HeaderValue::from_static("lytix"));
        headers
            .insert("alephant-omit-request", HeaderValue::from_static("true"));
        headers
            .insert("alephant-omit-response", HeaderValue::from_static("true"));

        let meta = AlephantLogMetadata::from_headers(
            &mut headers,
            None,
            &DeploymentTarget::default(),
            None,
        )
        .expect("metadata parsing should succeed");

        assert_eq!(meta.model_override.as_deref(), Some("gpt-4o-mini"));
        assert!(meta.webhook_enabled);
        assert!(meta.omit_request_log);
        assert!(meta.omit_response_log);
        assert_eq!(meta.posthog_api_key.as_deref(), Some("ph-key"));
        assert_eq!(meta.posthog_host.as_deref(), Some("https://ph.example"));
        assert_eq!(meta.lytix_key.as_deref(), Some("lytix"));
        assert!(!headers.contains_key("x-alephant-model-override"));
    }

    #[test]
    fn response_log_serializes_origin_prompt_tokens() {
        let r = ResponseLog::builder()
            .id(Uuid::nil())
            .status(200)
            .body_size(0.0)
            .latency(0)
            .time_to_first_token(0)
            .response_created_at(Utc::now())
            .origin_prompt_tokens(4_096)
            .prompt_tokens(2_048)
            .build();
        let value = serde_json::to_value(&r).expect("serialize response");
        assert_eq!(
            value
                .get("originPromptTokens")
                .and_then(serde_json::Value::as_i64),
            Some(4_096)
        );
        assert_eq!(
            value
                .get("promptTokens")
                .and_then(serde_json::Value::as_i64),
            Some(2_048)
        );
    }

    #[test]
    fn metadata_can_include_large_context_fields() {
        let mut headers = HeaderMap::new();
        let mut meta = AlephantLogMetadata::from_headers(
            &mut headers,
            None,
            &DeploymentTarget::default(),
            None,
        )
        .expect("metadata parsing should succeed");

        meta.apply_large_context_decision(&LargeContextDecision {
            handler: TokenLimitExceptionHandler::MiddleOut,
            action: LargeContextAction::MiddleOutApplied,
            original_model: Some("openai/gpt-4o-mini".to_string()),
            effective_model: Some("openai/gpt-4o-mini".to_string()),
            estimated_input_tokens: Some(120_000),
            model_context_limit: Some(128_000),
            input_budget_tokens: Some(115_200),
        });

        assert_eq!(meta.large_context_handler.as_deref(), Some("middle-out"));
        assert_eq!(
            meta.large_context_action.as_deref(),
            Some("middle-out-applied")
        );
        assert_eq!(
            meta.large_context_original_model.as_deref(),
            Some("openai/gpt-4o-mini")
        );
        assert_eq!(
            meta.large_context_effective_model.as_deref(),
            Some("openai/gpt-4o-mini")
        );
        assert_eq!(meta.large_context_estimated_tokens, Some(120_000));
        assert_eq!(meta.large_context_input_budget_tokens, Some(115_200));
    }

    #[test]
    fn log_message_redis_payload_matches_http_json_value() {
        let id = Uuid::nil();
        let user_id = UserId::new(Uuid::nil());
        let target_url: Url = "https://example.com/".parse().unwrap();
        let request_created_at = Utc::now();
        let request_log = RequestLog::builder()
            .id(id)
            .user_id(user_id)
            .workspace_id(OrgId::new(Uuid::nil()))
            .properties(IndexMap::new())
            .target_url(target_url)
            .provider("OPENAI".to_string())
            .model("gpt-4o".to_string())
            .body_size(42.0)
            .path("/v1/chat/completions".to_string())
            .request_created_at(request_created_at)
            .is_stream(false)
            .storage_location("s3".to_string())
            .build();
        let response_log = ResponseLog::builder()
            .id(Uuid::nil())
            .status(200)
            .body_size(0.0)
            .latency(100)
            .time_to_first_token(15)
            .response_created_at(Utc::now())
            .build();
        let msg = LogMessage::builder()
            .authorization("sk-test".into())
            .alephant_meta(AlephantLogMetadata::default())
            .log(Log::new(request_log, response_log))
            .build();
        let compact = serde_json::to_string(&msg).expect("serialize");
        assert!(
            !compact.contains('\n'),
            "Redis Stream payload must match compact HTTP JSON, got: {compact}",
        );
        let v: serde_json::Value = serde_json::from_str(&compact).unwrap();
        assert!(v.get("authorization").is_some());
        assert!(v.get("log").is_some());
    }

    #[test]
    fn kafka_log_serializes_key_fields() {
        let id = Uuid::nil();
        let user_id = UserId::new(Uuid::nil());
        let target_url: Url = "https://example.com/".parse().unwrap();
        let request_created_at = Utc::now();
        let request_log = RequestLog::builder()
            .id(id)
            .user_id(user_id)
            .workspace_id(OrgId::new(Uuid::nil()))
            .properties(IndexMap::new())
            .target_url(target_url)
            .provider("OPENAI".to_string())
            .model("gpt-4o".to_string())
            .body_size(42.0)
            .path("/v1/chat/completions".to_string())
            .request_created_at(request_created_at)
            .is_stream(false)
            .storage_location("s3".to_string())
            .build();
        let response_log = ResponseLog::builder()
            .id(Uuid::nil())
            .status(200)
            .body_size(0.0)
            .latency(100)
            .time_to_first_token(15)
            .response_created_at(Utc::now())
            .build();
        let json = serde_json::to_string(&Log::new(request_log, response_log))
            .unwrap();
        assert!(json.contains("\"workspaceId\""));
        assert!(
            json.contains("\"storageLocation\":\"s3\""),
            "Log.request must serialize storageLocation for downstream \
             ingest, got {json}",
        );
        assert!(
            !json.contains("sizeBytes"),
            "Log.request must not serialize sizeBytes (collector rejects \
             extras), got {json}",
        );
        assert!(
            json.contains("\"bodySize\":42"),
            "Log.request must serialize bodySize, got {json}",
        );
        assert!(
            !json.contains("\"model\""),
            "Log.request must not serialize model (use \
             alephantMeta.gatewayModel), got {json}",
        );
        assert!(json.contains("\"delayMs\":100"));
        assert!(json.contains("\"timeToFirstToken\":15"));
    }

    #[test]
    fn request_log_serializes_agent_fields_and_workspace_type_top_level() {
        let id = Uuid::nil();
        let user_id = UserId::new(Uuid::nil());
        let target_url: Url = "https://example.com/".parse().unwrap();
        let request_created_at = Utc::now();
        let agent_uid =
            Uuid::parse_str("01890f5a-52fd-7b9a-b51e-33a22f7b6f24").unwrap();

        let request_log = RequestLog::builder()
            .id(id)
            .user_id(user_id)
            .workspace_id(OrgId::new(Uuid::new_v4()))
            .workspace_type(Some("enterprise".to_string()))
            .properties(IndexMap::from([(
                "alephant-property-custom".to_string(),
                "keep".to_string(),
            )]))
            .alephant_agent_id(Some("coding-agent".to_string()))
            .alephant_agent_name(Some("Support Bot".to_string()))
            .alephant_agent_name_source(Some("virtual_key_label".to_string()))
            .alephant_agent_uid(Some(agent_uid))
            .alephant_run_id(Some("run-1".to_string()))
            .alephant_step_id(Some("step-1".to_string()))
            .alephant_parent_step_id(Some("step-0".to_string()))
            .alephant_tool_call_id(Some("call-1".to_string()))
            .alephant_handoff_id(Some("handoff-1".to_string()))
            .alephant_graph_node(Some("planner".to_string()))
            .alephant_iteration(Some(3))
            .alephant_state_hash(Some("sha256:abc".to_string()))
            .alephant_step_kind(Some("planning".to_string()))
            .alephant_step_source(Some("runtime".to_string()))
            .alephant_step_confidence(Some("high".to_string()))
            .alephant_agent_trust_level(Some("self_reported".to_string()))
            .target_url(target_url)
            .provider("OPENAI".to_string())
            .model("gpt-4o".to_string())
            .body_size(42.0)
            .path("/v1/chat/completions".to_string())
            .request_created_at(request_created_at)
            .is_stream(false)
            .build();

        let value =
            serde_json::to_value(&request_log).expect("serialize request log");

        assert_eq!(value["workspaceType"], "enterprise");
        assert_eq!(value["alephantAgentId"], "coding-agent");
        assert_eq!(value["alephantAgentName"], "Support Bot");
        assert_eq!(value["alephantAgentNameSource"], "virtual_key_label");
        assert_eq!(
            value["alephantAgentUid"],
            "01890f5a-52fd-7b9a-b51e-33a22f7b6f24"
        );
        assert_eq!(value["alephantRunId"], "run-1");
        assert_eq!(value["alephantStepId"], "step-1");
        assert_eq!(value["alephantParentStepId"], "step-0");
        assert_eq!(value["alephantToolCallId"], "call-1");
        assert_eq!(value["alephantHandoffId"], "handoff-1");
        assert_eq!(value["alephantGraphNode"], "planner");
        assert_eq!(value["alephantIteration"], 3);
        assert_eq!(value["alephantStateHash"], "sha256:abc");
        assert_eq!(value["alephantStepKind"], "planning");
        assert_eq!(value["alephantStepSource"], "runtime");
        assert_eq!(value["alephantStepConfidence"], "high");
        assert_eq!(value["alephantAgentTrustLevel"], "self_reported");

        let properties = value["properties"]
            .as_object()
            .expect("properties should be an object");
        assert!(properties.contains_key("alephant-property-custom"));
        assert!(!properties.contains_key("Alephant-Agent-Id"));
        assert!(!properties.contains_key("Alephant-Run-Id"));
        assert!(!properties.contains_key("Alephant-Step-Id"));
    }

    #[test]
    fn request_log_deserializes_legacy_organization_id_json_key() {
        let id = Uuid::nil();
        let user_id = UserId::new(Uuid::nil());
        let target_url: Url = "https://example.com/".parse().unwrap();
        let request_created_at = Utc::now();
        let built = RequestLog::builder()
            .id(id)
            .user_id(user_id)
            .workspace_id(OrgId::new(Uuid::new_v4()))
            .properties(IndexMap::new())
            .target_url(target_url)
            .provider("OPENAI".to_string())
            .model("gpt-4o".to_string())
            .body_size(0.0)
            .path("/v1/chat/completions".to_string())
            .request_created_at(request_created_at)
            .is_stream(false)
            .build();
        let mut value =
            serde_json::to_value(&built).expect("serialize RequestLog");
        let obj = value
            .as_object_mut()
            .expect("RequestLog serializes to a JSON object");
        let workspace = obj
            .remove("workspaceId")
            .expect("serialized RequestLog must contain workspaceId");
        obj.insert("organizationId".to_string(), workspace);
        let decoded: RequestLog = serde_json::from_value(value)
            .expect("deserialize with organizationId alias");
        assert_eq!(decoded.workspace_id, built.workspace_id);
    }

    #[test]
    fn request_log_deserializes_missing_storage_location_as_clickhouse() {
        let id = Uuid::nil();
        let user_id = UserId::new(Uuid::nil());
        let target_url: Url = "https://example.com/".parse().unwrap();
        let request_created_at = Utc::now();
        let built = RequestLog::builder()
            .id(id)
            .user_id(user_id)
            .workspace_id(OrgId::new(Uuid::new_v4()))
            .properties(IndexMap::new())
            .target_url(target_url)
            .provider("OPENAI".to_string())
            .model("gpt-4o".to_string())
            .body_size(0.0)
            .path("/v1/chat/completions".to_string())
            .request_created_at(request_created_at)
            .is_stream(false)
            .build();
        let mut value =
            serde_json::to_value(&built).expect("serialize RequestLog");
        let obj = value
            .as_object_mut()
            .expect("RequestLog serializes to a JSON object");
        obj.remove("storageLocation");
        let decoded: RequestLog = serde_json::from_value(value)
            .expect("deserialize without storageLocation");
        assert_eq!(decoded.storage_location, "clickhouse");
    }

    #[test]
    fn log_serializes_inline_bodies_under_body_key() {
        let id = Uuid::nil();
        let user_id = UserId::new(Uuid::nil());
        let target_url: Url = "https://example.com/".parse().unwrap();
        let request_created_at = Utc::now();
        let request_log = RequestLog::builder()
            .id(id)
            .user_id(user_id)
            .workspace_id(OrgId::new(Uuid::nil()))
            .properties(IndexMap::new())
            .target_url(target_url)
            .provider("OPENAI".to_string())
            .model("gpt-4o".to_string())
            .body_size(1.0)
            .path("/v1/chat/completions".to_string())
            .request_created_at(request_created_at)
            .is_stream(false)
            .storage_location("clickhouse".to_string())
            .request_body(r#"{"x":1}"#.to_string())
            .build();
        let response_log = ResponseLog::builder()
            .id(Uuid::nil())
            .status(200)
            .body_size(1.0)
            .latency(0)
            .time_to_first_token(0)
            .response_created_at(Utc::now())
            .response_body("{}".to_string())
            .build();
        let json = serde_json::to_string(&Log::new(request_log, response_log))
            .unwrap();
        assert!(
            json.contains(r#""body":"{\"x\":1}""#),
            "request should use JSON key body, got {json}",
        );
        assert!(
            json.contains(r#""body":"{}"#),
            "response should use JSON key body, got {json}",
        );
        assert!(
            !json.contains("requestBody") && !json.contains("responseBody"),
            "legacy body keys must not appear, got {json}",
        );
    }
}
