use bytes::Bytes;

use super::{
    capabilities::ProviderCapabilities,
    params::OpenAiRequestParams,
    profile_resolver::ResolvedMapperMetadata,
    rules::{ProviderRuleSet, RequestRuleContext},
};
use crate::{
    endpoints::{ApiEndpoint, openai::OpenAI},
    error::{api::ApiError, invalid_req::InvalidRequestError},
    types::{model_id::ModelId, provider::InferenceProvider},
};

#[derive(Debug, Clone)]
pub struct RequestEnvelope {
    pub source_endpoint: ApiEndpoint,
    pub target_provider: InferenceProvider,
    pub target_capabilities: Option<ProviderCapabilities>,
    pub target_rules: Option<ProviderRuleSet>,
    pub request_rule_context: Option<RequestRuleContext>,
    pub resolved_metadata: Option<ResolvedMapperMetadata>,
    pub raw_model: String,
    pub source_model: Option<ModelId>,
    pub is_stream: bool,
    pub openai_request: async_openai::types::CreateChatCompletionRequest,
}

impl RequestEnvelope {
    #[must_use]
    pub fn from_openai_chat_request(
        source_endpoint: ApiEndpoint,
        target_provider: InferenceProvider,
        mut request: async_openai::types::CreateChatCompletionRequest,
    ) -> Self {
        let params = OpenAiRequestParams::from_request(&request);

        #[allow(deprecated)]
        if request.max_completion_tokens.is_none()
            && let Some(v) = request.max_tokens.take()
        {
            request.max_completion_tokens = Some(v);
        }

        Self {
            source_endpoint,
            target_provider,
            target_capabilities: None,
            target_rules: None,
            request_rule_context: None,
            resolved_metadata: None,
            raw_model: params.raw_model,
            source_model: params.source_model,
            is_stream: params.is_stream,
            openai_request: request,
        }
    }

    #[must_use]
    pub fn with_target_capabilities(mut self, capabilities: ProviderCapabilities) -> Self {
        self.target_capabilities = Some(capabilities);
        self
    }

    #[must_use]
    pub fn with_target_rules(mut self, rules: ProviderRuleSet) -> Self {
        self.target_rules = Some(rules);
        self
    }

    #[must_use]
    pub fn with_request_rule_context(mut self, request_rule_context: RequestRuleContext) -> Self {
        self.request_rule_context = Some(request_rule_context);
        self
    }

    #[must_use]
    pub fn with_resolved_metadata(mut self, resolved_metadata: ResolvedMapperMetadata) -> Self {
        self.resolved_metadata = Some(resolved_metadata);
        self
    }

    pub fn from_source_request_bytes(
        source_endpoint: &ApiEndpoint,
        target_provider: InferenceProvider,
        body: &Bytes,
    ) -> Result<Option<Self>, ApiError> {
        match source_endpoint {
            ApiEndpoint::OpenAI(openai) if *openai == OpenAI::chat_completions() => {
                let request = serde_json::from_slice::<
                    async_openai::types::CreateChatCompletionRequest,
                >(body)
                .map_err(InvalidRequestError::InvalidRequestBody)?;

                Ok(Some(Self::from_openai_chat_request(
                    source_endpoint.clone(),
                    target_provider,
                    request,
                )))
            }
            _ => Ok(None),
        }
    }
}
