use std::{
    collections::HashMap,
    sync::{Arc, atomic::AtomicBool},
};

use async_openai::types::responses::CreateResponse;
use derive_more::{AsRef, From, Into};
use uuid::Uuid;

use super::{model_id::ModelId, org::OrgId, user::UserId};
use crate::{
    agent::context::AgentContext,
    config::router::RouterConfig,
    middleware::{
        large_context::headers::TokenLimitExceptionHandler,
        mapper::non_stream_profile::NonStreamFormatProfile,
    },
    types::{provider::InferenceProvider, secret::Secret},
};

#[derive(Debug, Clone, AsRef, From, Into)]
pub struct ProviderRequestId(pub(crate) http::HeaderValue);

/// Per-request snapshot of the virtual key's access-control and rate-limit
/// configuration, captured by the auth middleware.  Only present in Cloud
/// mode when the request was authenticated with a virtual key.
#[derive(Debug, Clone)]
pub struct VkPolicy {
    pub virtual_key_id: Uuid,
    pub allowed_models: Option<Vec<String>>,
    pub blocked_models: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
pub struct AuthContext {
    pub api_key: Secret<String>,
    pub user_id: UserId,
    /// Maps to `workspace_id` in the new `virtual_keys` schema.
    pub org_id: OrgId,
    /// `workspaces.type::text` resolved during virtual-key auth when PG
    /// enrichment is available.
    pub workspace_type: Option<String>,
    /// The row `id` from `virtual_keys`.
    pub virtual_key_id: Option<Uuid>,
    /// `virtual_keys.key_prefix` (public prefix for display / RMT).
    pub virtual_key_prefix: String,
    /// The `master_key_id` from `virtual_keys`.
    pub master_key_id: Option<Uuid>,
    /// Optional `master_keys.base_url` override for the upstream provider.
    /// When set, the Dispatcher uses it instead of
    /// `config.providers[*].base_url`. `None` when the master key has no
    /// custom URL.
    pub master_key_base_url: Option<String>,
    /// Department for RMT / policy / logs: resolved at VK auth from
    /// agent/member (`router_store`); all-zero UUID when unknown or lookup
    /// failed.
    pub department_id: Uuid,
    /// `member` \| `agent` or empty when unknown (logs-collector
    /// `LowCardinality`).
    pub entity_type: String,
    pub entity_id: Uuid,
    /// Display name for logs list; empty when unknown.
    pub entity_name: String,
    /// Agent display name derived from the authenticated VK label
    /// `agent:<name>`. This is display metadata only; identity, policy, and
    /// billing must continue to use stable IDs.
    pub registered_agent_name: Option<String>,
    /// RMT `body_ttl_days`: from workspace active `subscriptions.log_limit`
    /// (clamped to 1–730).
    pub body_ttl_days: u16,
    /// `true` when the VK's master_key is bound to `providers.code =
    /// 'custom'`. Downstream middleware uses this to skip model validation
    /// and route directly via `master_key_base_url`.
    pub is_custom_provider: bool,
    /// When authenticated with a virtual key and the master key was resolved
    /// from [`MasterKeyCache`], lists providers this master key may use for
    /// Unified API routing. `None` when cache miss, no cache, or not
    /// applicable. Phase 1: at most one element; multi-provider keys will
    /// populate multiple entries.
    ///
    /// [`MasterKeyCache`]: crate::store::master_key_cache::MasterKeyCache
    pub master_key_allowed_providers: Option<Vec<InferenceProvider>>,
}

#[derive(Debug)]
pub struct RequestContext {
    /// If `None`, the request was for a direct proxy.
    /// If `Some`, the request was for a load balanced router.
    pub router_config: Option<Arc<RouterConfig>>,
    /// If `None`, the router is configured to not require auth for requests,
    /// disabling some features.
    pub auth_context: Option<AuthContext>,
    /// When false, skip LLM KV cache read (org / product policy hook).
    pub llm_kv_cache_read_allowed: bool,
    /// When false, skip LLM KV cache write.
    pub llm_kv_cache_write_allowed: bool,
    /// Agent Gateway context parsed from headers or system-derived metadata.
    pub agent_context: Option<AgentContext>,
}

#[derive(Debug, Default, Clone)]
pub struct AnthropicStreamOpenAiUsageState {
    pub stream_message_id: String,
    pub stream_model: String,
    pub input_tokens: u32,
    pub cache_read_input_tokens: u32,
    pub cache_creation_input_tokens: u32,
    pub cache_ephemeral_5m: u32,
    pub cache_ephemeral_1h: u32,
    pub output_tokens: u32,
}

/// Per-response Anthropic→OpenAI stream usage aggregation (streaming only).
pub type AnthropicOpenAiUsageCell =
    std::sync::Arc<std::sync::Mutex<AnthropicStreamOpenAiUsageState>>;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ClientResponseSemantic {
    #[default]
    Other,
    ChatCompletions,
    Responses,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LoggerResponseWireSemantic {
    #[default]
    Other,
    ChatCompletionsJson,
    ChatCompletionsSse,
    ResponsesJson,
    ResponsesSse,
}

#[derive(Debug, Clone)]
pub struct MapperContext {
    pub is_stream: bool,
    pub client_response_semantic: ClientResponseSemantic,
    pub logger_response_wire_semantic: LoggerResponseWireSemantic,
    /// If `None`, the request was for an endpoint without
    /// first class support for mapping between different provider
    /// models.
    pub model: Option<ModelId>,
    /// When `is_stream` is true, holds cross-chunk Anthropic usage for the
    /// OpenAI-mapped SSE response; `None` for non-streaming.
    pub anthropic_openai_usage: Option<AnthropicOpenAiUsageCell>,
    /// Unified `chat/completions` used Responses-shaped JSON and was routed to
    /// `/v1/responses`; IDE adaptation maps upstream Responses SSE / JSON back
    /// to Chat Completions for the client (e.g. Cursor).
    pub unified_responses_bridge_chat_completions_sse: bool,
    /// When true, mapper skipped semantic body conversion for this
    /// round-trip; response mapping reads this from [`MapperContext`]
    /// copied onto the upstream [`http::Response`].
    pub native_semantic_passthrough: bool,
    /// Cursor IDE + OpenAI-compatible upstream: client called `/v1/responses`
    /// but the gateway translated the request to `chat/completions`; map the
    /// upstream Chat Completions stream / JSON back to Responses API shape.
    pub cursor_responses_via_chat_completions: bool,
    /// Original client [`CreateResponse`] when
    /// [`Self::cursor_responses_via_chat_completions`] is enabled.
    pub cursor_responses_origin: Option<CreateResponse>,
    /// Client expects Responses API wire on the HTTP response (e.g. Codex VS
    /// Code). When true, do not use the unified `chat/completions` shortcut
    /// that returns upstream Chat Completions SSE unchanged.
    pub client_expects_responses_wire: bool,
}

/// Request extension: mapper skips
/// [`crate::middleware::mapper::EndpointConverter`] semantic body conversion
/// for this round-trip when paired with
/// [`MapperContext::native_semantic_passthrough`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeSemanticPassthrough;

/// Marker: unified `chat/completions` redirected to Responses API due to
/// Responses-shaped body; IDE adaptation bridges the upstream stream / JSON
/// back to Chat Completions for the client.
#[derive(Debug, Clone, Copy)]
pub struct UnifiedChatCompletionsResponsesBridge;

#[derive(Debug, Clone)]
pub struct MapperProfileContext {
    pub provider: crate::types::provider::InferenceProvider,
    pub raw_model: String,
    pub non_stream_profile: NonStreamFormatProfile,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct PromptContext {
    pub prompt_id: String,
    pub prompt_version_id: Option<String>,
    pub inputs: Option<HashMap<String, serde_json::Value>>,
}

/// When `Alephant-Prompt-ID` + Redis `prompt:cache` hit, fills request log
/// `crate::types::logger::RequestLog` fields `prompt_id` / `prompt_version`.
/// If [`PromptContext`] is also present, `LoggerService` prefers this struct
/// (design G4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptHeaderForRequestLog {
    pub prompt_id: String,
    pub prompt_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnifiedImplicitModelFallbackContext {
    pub selected_model: String,
}

/// Unified API: `model` prefix missed [`crate::router::direct::DirectProxies`];
/// request forwards via custom [`AuthContext::master_key_base_url`] without
/// catalog / `model-mapping` resolution (body `model` kept verbatim).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MasterKeyUnifiedModelPassthrough;

/// Unified API has already enforced virtual-key model policy for this request.
/// Mapper must not run allowed_models / blocked_models a second time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UnifiedModelPolicyChecked;

/// Unified API explicit model should bypass provider catalog/model mapping.
/// This does not change the target provider, is not
/// [`NativeSemanticPassthrough`], is not [`MasterKeyUnifiedModelPassthrough`],
/// and does not bypass IDE/request/ response adaptation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UnifiedModelBodyPassthrough;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LargeContextAction {
    SkippedNoModel,
    SkippedNoTextMessages,
    SkippedNonTextMessages,
    SkippedNoEstimate,
    SkippedBelowLimit,
    SkippedNoModelLimit,
    SkippedNoFallbackModel,
    Truncated,
    MiddleOutApplied,
    FallbackApplied,
}

impl LargeContextAction {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SkippedNoModel => "skipped-no-model",
            Self::SkippedNoTextMessages => "skipped-no-text-messages",
            Self::SkippedNonTextMessages => "skipped-non-text-messages",
            Self::SkippedNoEstimate => "skipped-no-estimate",
            Self::SkippedBelowLimit => "skipped-below-limit",
            Self::SkippedNoModelLimit => "skipped-no-model-limit",
            Self::SkippedNoFallbackModel => "skipped-no-fallback-model",
            Self::Truncated => "truncated",
            Self::MiddleOutApplied => "middle-out-applied",
            Self::FallbackApplied => "fallback-applied",
        }
    }
}

#[derive(Debug, Clone)]
pub struct LargeContextDecision {
    pub handler: TokenLimitExceptionHandler,
    pub action: LargeContextAction,
    pub original_model: Option<String>,
    pub effective_model: Option<String>,
    pub estimated_input_tokens: Option<u32>,
    pub model_context_limit: Option<u32>,
    pub input_budget_tokens: Option<u32>,
}

/// Gateway-side prompt compression token estimates before and after
/// compression, used by `alephantMeta` and the logger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromptCompressionTokenPair {
    pub origin_prompt_token: u32,
    pub compression_prompt_token: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestKind {
    Router,
    UnifiedApi,
    DirectProxy,
    CustomProvider,
    AgentEvents,
    X402Agent,
}

/// Shared flag between the fallback request-log middleware and precise
/// logging paths (`handle_logging`, `emit_policy_deny_request_log`,
/// `emit_mapper_policy_deny_log`).  Set to `true` **before** spawning
/// the log task so the middleware sees it immediately after `inner.call()`
/// returns.
#[derive(Debug, Clone)]
pub struct RequestLogEmitted(pub Arc<AtomicBool>);

impl RequestLogEmitted {
    #[must_use]
    pub fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

    pub fn mark(&self) {
        self.0.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    #[must_use]
    pub fn is_emitted(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl Default for RequestLogEmitted {
    fn default() -> Self {
        Self::new()
    }
}
