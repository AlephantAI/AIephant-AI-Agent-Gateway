use std::str::FromStr;

use bytes::Bytes;
use http::response::Parts;
use serde_json::Value;

use super::{
    TryConvertStreamData, capabilities::ProviderCapabilities, model::ModelMapper,
    non_stream_profile::NonStreamFormatProfile,
    non_stream_profile_data::default_non_stream_profile, params::OpenAiRequestParams,
    rules::ProviderRuleSet,
};
use crate::{
    endpoints::openai::OpenAICompatibleChatCompletionRequest,
    error::mapper::MapperError,
    middleware::mapper::{ResponseBodyConverter, TryConvert, TryConvertError},
    types::{model_id::ModelId, provider::InferenceProvider},
};

#[derive(Debug)]
pub struct OpenAICompatibleConverter {
    provider: InferenceProvider,
    capabilities: ProviderCapabilities,
    rules: ProviderRuleSet,
    non_stream_profile: NonStreamFormatProfile,
    model_mapper: ModelMapper,
}

impl OpenAICompatibleConverter {
    fn derived_profile_from_metadata(
        provider: &InferenceProvider,
        rules: &ProviderRuleSet,
    ) -> NonStreamFormatProfile {
        let mut non_stream_profile = default_non_stream_profile(provider);
        non_stream_profile.request.tool_choice_mode = rules.request.tool_choice_mode;
        non_stream_profile.request.response_format_mode = rules.request.response_format_mode;
        non_stream_profile.request.reasoning_mode = rules.request.reasoning_mode;
        non_stream_profile.request.multimodal_mode = rules.request.multimodal_mode;
        non_stream_profile
    }

    #[must_use]
    pub fn new(provider: InferenceProvider, model_mapper: ModelMapper) -> Self {
        let capabilities = ProviderCapabilities::for_provider(&provider);
        let rules = super::rule_data::default_provider_rules(&provider);
        let non_stream_profile = Self::derived_profile_from_metadata(&provider, &rules);
        Self::new_with_profile_metadata(
            provider,
            capabilities,
            rules,
            non_stream_profile,
            model_mapper,
        )
    }

    #[must_use]
    pub fn new_with_metadata(
        provider: InferenceProvider,
        capabilities: ProviderCapabilities,
        rules: ProviderRuleSet,
        model_mapper: ModelMapper,
    ) -> Self {
        let non_stream_profile = Self::derived_profile_from_metadata(&provider, &rules);
        Self::new_with_profile_metadata(
            provider,
            capabilities,
            rules,
            non_stream_profile,
            model_mapper,
        )
    }

    #[must_use]
    pub fn new_with_profile_metadata(
        provider: InferenceProvider,
        capabilities: ProviderCapabilities,
        rules: ProviderRuleSet,
        non_stream_profile: NonStreamFormatProfile,
        model_mapper: ModelMapper,
    ) -> Self {
        Self::try_new_with_profile_metadata(
            provider,
            capabilities,
            rules,
            non_stream_profile,
            model_mapper,
        )
        .expect("openai-compatible converter metadata must be valid")
    }

    pub fn try_new_with_metadata(
        provider: InferenceProvider,
        capabilities: ProviderCapabilities,
        rules: ProviderRuleSet,
        model_mapper: ModelMapper,
    ) -> Result<Self, MapperError> {
        let non_stream_profile = Self::derived_profile_from_metadata(&provider, &rules);
        Self::try_new_with_profile_metadata(
            provider,
            capabilities,
            rules,
            non_stream_profile,
            model_mapper,
        )
    }

    pub fn try_new_with_profile_metadata(
        provider: InferenceProvider,
        capabilities: ProviderCapabilities,
        rules: ProviderRuleSet,
        non_stream_profile: NonStreamFormatProfile,
        model_mapper: ModelMapper,
    ) -> Result<Self, MapperError> {
        if !capabilities.openai_compatible
            || !matches!(
                rules.family,
                super::families::ProviderProtocolFamily::OpenAiCompatible
            )
        {
            return Err(MapperError::ProviderNotSupported(provider.to_string()));
        }

        if non_stream_profile.provider != provider || non_stream_profile.family != rules.family {
            return Err(MapperError::ProviderNotSupported(provider.to_string()));
        }

        Ok(Self {
            provider,
            capabilities,
            rules,
            non_stream_profile,
            model_mapper,
        })
    }
}

fn is_openrouter_provider(provider: &InferenceProvider) -> bool {
    matches!(
        provider,
        InferenceProvider::Named(name) if name.as_str() == "openrouter"
    )
}

fn responses_has_function_tool(root: &Value, tool_name: &str) -> bool {
    root.get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| {
            tools.iter().any(|tool| {
                tool.get("type").and_then(Value::as_str) == Some("function")
                    && tool.get("name").and_then(Value::as_str) == Some(tool_name)
            })
        })
}

fn patch_openrouter_responses_tool_choice(bytes: Bytes) -> Result<Bytes, MapperError> {
    let mut root: Value = serde_json::from_slice(&bytes).map_err(MapperError::SerdeError)?;

    let Some(tool_choice) = root.get("tool_choice") else {
        return Ok(bytes);
    };
    if !tool_choice.is_object() {
        return Ok(bytes);
    }
    if tool_choice.get("type").is_some() {
        return Ok(bytes);
    }

    let Some(tool_name) = tool_choice
        .get("name")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    else {
        return Ok(bytes);
    };
    if !responses_has_function_tool(&root, tool_name) {
        return Ok(bytes);
    }

    let Some(tool_choice) = root.get_mut("tool_choice") else {
        return Ok(bytes);
    };
    let Some(obj) = tool_choice.as_object_mut() else {
        return Ok(bytes);
    };
    obj.insert("type".to_string(), Value::String("function".to_string()));

    serde_json::to_vec(&root)
        .map(Bytes::from)
        .map_err(MapperError::SerdeError)
}

impl
    TryConvert<
        async_openai::types::CreateChatCompletionRequest,
        OpenAICompatibleChatCompletionRequest,
    > for OpenAICompatibleConverter
{
    type Error = MapperError;
    fn try_convert(
        &self,
        mut value: async_openai::types::CreateChatCompletionRequest,
    ) -> Result<OpenAICompatibleChatCompletionRequest, Self::Error> {
        if !self.capabilities.openai_compatible
            || !matches!(
                self.rules.family,
                super::families::ProviderProtocolFamily::OpenAiCompatible
            )
        {
            return Err(MapperError::ProviderNotSupported(self.provider.to_string()));
        }

        #[allow(deprecated)]
        if value.max_completion_tokens.is_none()
            && let Some(v) = value.max_tokens.take()
        {
            value.max_completion_tokens = Some(v);
        }

        if self.model_mapper.target_skips_model_catalog(&self.provider) {
            tracing::trace!(
                raw_model = %value.model,
                provider = %self.provider,
                "openai-compatible model aggregator: skip catalog mapping, pass through model"
            );
        } else {
            let source_model = OpenAiRequestParams::from_request(&value)
                .source_model
                .ok_or_else(|| MapperError::InvalidModelName(value.model.clone()))?;
            let target_model = self.model_mapper.map_model(&source_model, &self.provider)?;
            tracing::trace!(source_model = ?source_model, target_model = ?target_model, "mapped model");
            value.model = target_model.to_string();
        }

        if value.stream.unwrap_or(false) && value.stream_options.is_none() {
            value.stream_options = Some(async_openai::types::ChatCompletionStreamOptions {
                include_usage: true,
            });
        }

        Ok(OpenAICompatibleChatCompletionRequest {
            provider: self.provider.clone(),
            inner: value,
        })
    }

    fn try_convert_model_passthrough(
        &self,
        mut value: async_openai::types::CreateChatCompletionRequest,
    ) -> Result<OpenAICompatibleChatCompletionRequest, Self::Error> {
        if !self.capabilities.openai_compatible
            || !matches!(
                self.rules.family,
                super::families::ProviderProtocolFamily::OpenAiCompatible
            )
        {
            return Err(MapperError::ProviderNotSupported(self.provider.to_string()));
        }

        #[allow(deprecated)]
        if value.max_completion_tokens.is_none() {
            if let Some(v) = value.max_tokens.take() {
                value.max_completion_tokens = Some(v);
            }
        }

        Ok(OpenAICompatibleChatCompletionRequest {
            provider: self.provider.clone(),
            inner: value,
        })
    }
}

impl
    TryConvert<
        async_openai::types::CreateChatCompletionResponse,
        async_openai::types::CreateChatCompletionResponse,
    > for OpenAICompatibleConverter
{
    type Error = MapperError;
    fn try_convert(
        &self,
        value: async_openai::types::CreateChatCompletionResponse,
    ) -> Result<async_openai::types::CreateChatCompletionResponse, Self::Error> {
        Ok(value)
    }
}

impl
    TryConvert<
        async_openai::types::CreateEmbeddingRequest,
        async_openai::types::CreateEmbeddingRequest,
    > for OpenAICompatibleConverter
{
    type Error = MapperError;
    fn try_convert(
        &self,
        mut value: async_openai::types::CreateEmbeddingRequest,
    ) -> Result<async_openai::types::CreateEmbeddingRequest, Self::Error> {
        if self.model_mapper.target_skips_model_catalog(&self.provider) {
            tracing::trace!(
                raw_model = %value.model,
                provider = %self.provider,
                "openai-compatible embeddings: skip catalog, pass through model"
            );
        } else {
            let source_model = ModelId::from_str(&value.model)?;
            let target_model = self.model_mapper.map_model(&source_model, &self.provider)?;
            tracing::trace!(
                source_model = ?source_model,
                target_model = ?target_model,
                "openai-compatible embeddings: mapped model"
            );
            value.model = target_model.to_string();
        }
        Ok(value)
    }

    fn try_convert_model_passthrough(
        &self,
        value: async_openai::types::CreateEmbeddingRequest,
    ) -> Result<async_openai::types::CreateEmbeddingRequest, Self::Error> {
        Ok(value)
    }
}

impl
    TryConvert<
        async_openai::types::CreateEmbeddingResponse,
        async_openai::types::CreateEmbeddingResponse,
    > for OpenAICompatibleConverter
{
    type Error = MapperError;
    fn try_convert(
        &self,
        value: async_openai::types::CreateEmbeddingResponse,
    ) -> Result<async_openai::types::CreateEmbeddingResponse, Self::Error> {
        Ok(value)
    }
}

impl
    ResponseBodyConverter<
        async_openai::types::CreateEmbeddingResponse,
        async_openai::types::CreateEmbeddingResponse,
    > for OpenAICompatibleConverter
{
}

impl
    TryConvertStreamData<
        async_openai::types::CreateEmbeddingResponse,
        async_openai::types::CreateEmbeddingResponse,
    > for OpenAICompatibleConverter
{
    type Error = MapperError;
    fn try_convert_chunk(
        &self,
        value: async_openai::types::CreateEmbeddingResponse,
        _anthropic_openai_usage: Option<&crate::types::extensions::AnthropicOpenAiUsageCell>,
    ) -> Result<Option<async_openai::types::CreateEmbeddingResponse>, Self::Error> {
        Ok(Some(value))
    }
}

impl
    ResponseBodyConverter<
        async_openai::types::CreateChatCompletionResponse,
        async_openai::types::CreateChatCompletionResponse,
    > for OpenAICompatibleConverter
{
    fn try_convert_response(
        &self,
        resp_parts: &Parts,
        value: async_openai::types::CreateChatCompletionResponse,
    ) -> Result<async_openai::types::CreateChatCompletionResponse, Self::Error> {
        super::non_stream_response_interpreter::apply_non_stream_response_profile(
            super::non_stream_response_interpreter::profile_from_response_parts(
                resp_parts,
                &self.non_stream_profile,
            ),
            value,
        )
    }
}

impl
    TryConvertStreamData<
        async_openai::types::CreateChatCompletionStreamResponse,
        async_openai::types::CreateChatCompletionStreamResponse,
    > for OpenAICompatibleConverter
{
    type Error = MapperError;

    fn try_convert_chunk(
        &self,
        value: async_openai::types::CreateChatCompletionStreamResponse,
        _anthropic_openai_usage: Option<&crate::types::extensions::AnthropicOpenAiUsageCell>,
    ) -> Result<Option<async_openai::types::CreateChatCompletionStreamResponse>, Self::Error> {
        Ok(Some(value))
    }
}

impl TryConvertError<async_openai::error::WrappedError, async_openai::error::WrappedError>
    for OpenAICompatibleConverter
{
    type Error = MapperError;

    fn try_convert_error(
        &self,
        _resp_parts: &Parts,
        value: async_openai::error::WrappedError,
    ) -> Result<async_openai::error::WrappedError, Self::Error> {
        Ok(value)
    }
}

impl
    TryConvert<
        async_openai::types::responses::CreateResponse,
        async_openai::types::responses::CreateResponse,
    > for OpenAICompatibleConverter
{
    type Error = MapperError;
    fn try_convert(
        &self,
        mut value: async_openai::types::responses::CreateResponse,
    ) -> Result<async_openai::types::responses::CreateResponse, Self::Error> {
        if self.model_mapper.target_skips_model_catalog(&self.provider) {
            tracing::trace!(
                raw_model = %value.model,
                provider = %self.provider,
                "openai-compatible responses: skip catalog, pass through model"
            );
        } else {
            let source_model = ModelId::from_str(&value.model)?;
            let target_model = self.model_mapper.map_model(&source_model, &self.provider)?;
            tracing::trace!(
                source_model = ?source_model,
                target_model = ?target_model,
                "openai-compatible responses: mapped model"
            );
            value.model = target_model.to_string();
        }
        Ok(value)
    }

    fn try_convert_model_passthrough(
        &self,
        value: async_openai::types::responses::CreateResponse,
    ) -> Result<async_openai::types::responses::CreateResponse, Self::Error> {
        Ok(value)
    }

    fn postprocess_converted_req_body(&self, bytes: Bytes) -> Result<Bytes, Self::Error> {
        if is_openrouter_provider(&self.provider) {
            patch_openrouter_responses_tool_choice(bytes)
        } else {
            Ok(bytes)
        }
    }
}

impl TryConvert<async_openai::types::responses::Response, async_openai::types::responses::Response>
    for OpenAICompatibleConverter
{
    type Error = MapperError;
    fn try_convert(
        &self,
        value: async_openai::types::responses::Response,
    ) -> Result<async_openai::types::responses::Response, Self::Error> {
        Ok(value)
    }
}

impl
    ResponseBodyConverter<
        async_openai::types::responses::Response,
        async_openai::types::responses::Response,
    > for OpenAICompatibleConverter
{
}

impl
    TryConvertStreamData<
        async_openai::types::responses::Response,
        async_openai::types::responses::Response,
    > for OpenAICompatibleConverter
{
    type Error = MapperError;
    fn try_convert_chunk(
        &self,
        value: async_openai::types::responses::Response,
        _anthropic_openai_usage: Option<&crate::types::extensions::AnthropicOpenAiUsageCell>,
    ) -> Result<Option<async_openai::types::responses::Response>, Self::Error> {
        Ok(Some(value))
    }
}

#[cfg(test)]
mod tests {
    use async_openai::types::{CreateChatCompletionRequest, CreateEmbeddingRequest};
    use indexmap::IndexSet;
    use rustc_hash::FxHashMap;
    use serde_json::json;
    use url::Url;

    use crate::{
        config::providers::GlobalProviderConfig,
        middleware::mapper::{
            capabilities::ProviderCapabilities, model::ModelMapper,
            rule_data::default_provider_rules, rules::ProviderRuleSet,
        },
        types::{model_id::ModelId, provider::InferenceProvider},
    };

    fn sample_request() -> CreateChatCompletionRequest {
        serde_json::from_value(json!({
            "model": "custom-openai/gpt-4o-mini",
            "messages": [
                {
                    "role": "user",
                    "content": "hello"
                }
            ]
        }))
        .expect("request should deserialize")
    }

    fn named_provider_config() -> GlobalProviderConfig {
        GlobalProviderConfig {
            models: IndexSet::from_iter([
                ModelId::from_str_and_provider(
                    InferenceProvider::Named("custom-openai".into()),
                    "gpt-4o-mini",
                )
                .expect("model should parse"),
                ModelId::from_str_and_provider(
                    InferenceProvider::Named("custom-openai".into()),
                    "text-embedding-3-small",
                )
                .expect("model should parse"),
            ]),
            base_url: Url::parse("http://127.0.0.1:8011/v1").expect("url should parse"),
            cn_base_url: None,
            version: None,
            upstream_auth: Default::default(),
        }
    }

    fn sample_responses_request() -> async_openai::types::responses::CreateResponse {
        async_openai::types::responses::CreateResponse {
            model: "custom-openai/gpt-4o-mini".to_string(),
            input: async_openai::types::responses::Input::Text("hello".to_string()),
            ..Default::default()
        }
    }

    fn sample_embedding_request() -> CreateEmbeddingRequest {
        serde_json::from_value(json!({
            "model": "custom-openai/text-embedding-3-small",
            "input": "hello"
        }))
        .expect("embedding request should deserialize")
    }

    fn convert_responses_body(
        converter: super::OpenAICompatibleConverter,
        body: serde_json::Value,
    ) -> serde_json::Value {
        let bytes =
            bytes::Bytes::from(serde_json::to_vec(&body).expect("request body should serialize"));
        let typed = crate::middleware::mapper::TypedEndpointConverter::<
            crate::endpoints::openai::Responses,
            crate::endpoints::openai::Responses,
            super::OpenAICompatibleConverter,
        >::new(converter);
        let (converted, _ctx) =
            crate::middleware::mapper::EndpointConverter::convert_req_body(&typed, bytes)
                .expect("request should convert");
        serde_json::from_slice(&converted).expect("converted body should be JSON")
    }

    fn sample_multimodal_request() -> CreateChatCompletionRequest {
        serde_json::from_value(json!({
            "model": "custom-openai/gpt-4o-mini",
            "messages": [
                {
                    "role": "user",
                    "content": [
                        {
                            "type": "text",
                            "text": "describe the image"
                        },
                        {
                            "type": "image_url",
                            "image_url": {
                                "url": "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/a1sAAAAASUVORK5CYII="
                            }
                        }
                    ]
                }
            ]
        }))
        .expect("multimodal request should deserialize")
    }

    #[tokio::test]
    async fn openai_compatible_converter_can_be_built_from_explicit_metadata() {
        let mut config = crate::config::Config::default();
        let named_provider = InferenceProvider::Named("custom-openai".into());
        config
            .providers
            .insert(named_provider.clone(), named_provider_config());
        let app = crate::app::build_test_app(config).await.expect("build app");
        let model_mapper = ModelMapper::new(app.state.clone());
        let capabilities = ProviderCapabilities::for_provider(&named_provider);
        let rules = default_provider_rules(&named_provider);

        let converter = super::OpenAICompatibleConverter::new_with_metadata(
            named_provider.clone(),
            capabilities,
            rules,
            model_mapper,
        );
        let converted =
            crate::middleware::mapper::TryConvert::try_convert(&converter, sample_request())
                .expect("conversion should succeed");

        assert_eq!(converted.provider, named_provider);
        assert_eq!(converted.inner.model, "gpt-4o-mini");
    }

    #[tokio::test]
    async fn openrouter_skips_model_catalog_and_preserves_raw_model_string() {
        let config = crate::config::Config::default();
        let app = crate::app::build_test_app(config).await.expect("build app");
        let mut flags = FxHashMap::default();
        flags.insert("openai".to_string(), false);
        flags.insert("openrouter".to_string(), true);
        app.state.set_provider_is_router_flags(flags);
        let openrouter = InferenceProvider::Named("openrouter".into());
        let model_mapper = ModelMapper::new(app.state.clone());
        let capabilities = ProviderCapabilities::for_provider(&openrouter);
        let rules = default_provider_rules(&openrouter);
        let converter = super::OpenAICompatibleConverter::new_with_metadata(
            openrouter.clone(),
            capabilities,
            rules,
            model_mapper,
        );
        let request: CreateChatCompletionRequest = serde_json::from_value(json!({
            "model": "openai/gpt-4",
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .expect("request should deserialize");

        let converted = crate::middleware::mapper::TryConvert::try_convert(&converter, request)
            .expect("conversion should succeed");

        assert_eq!(converted.inner.model, "openai/gpt-4");
    }

    #[tokio::test]
    async fn openrouter_skips_model_parsing_for_bare_chat_model() {
        let config = crate::config::Config::default();
        let app = crate::app::build_test_app(config).await.expect("build app");
        let mut flags = FxHashMap::default();
        flags.insert("openrouter".to_string(), true);
        app.state.set_provider_is_router_flags(flags);
        let openrouter = InferenceProvider::Named("openrouter".into());
        let model_mapper = ModelMapper::new(app.state.clone());
        let capabilities = ProviderCapabilities::for_provider(&openrouter);
        let rules = default_provider_rules(&openrouter);
        let converter = super::OpenAICompatibleConverter::new_with_metadata(
            openrouter.clone(),
            capabilities,
            rules,
            model_mapper,
        );
        let request: CreateChatCompletionRequest = serde_json::from_value(json!({
            "model": "gpt-5.4",
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .expect("request should deserialize");

        let converted = crate::middleware::mapper::TryConvert::try_convert(&converter, request)
            .expect("conversion should succeed");

        assert_eq!(converted.provider, openrouter);
        assert_eq!(converted.inner.model, "gpt-5.4");
    }

    #[tokio::test]
    async fn openrouter_when_db_snapshot_marks_not_router_runs_catalog_mapping() {
        let config = crate::config::Config::default();
        let app = crate::app::build_test_app(config).await.expect("build app");
        let mut flags = FxHashMap::default();
        flags.insert("openrouter".to_string(), false);
        app.state.set_provider_is_router_flags(flags);
        let openrouter = InferenceProvider::Named("openrouter".into());
        let model_mapper = ModelMapper::new(app.state.clone());
        let capabilities = ProviderCapabilities::for_provider(&openrouter);
        let rules = default_provider_rules(&openrouter);
        let converter = super::OpenAICompatibleConverter::new_with_metadata(
            openrouter.clone(),
            capabilities,
            rules,
            model_mapper,
        );
        let request: CreateChatCompletionRequest = serde_json::from_value(json!({
            "model": "openai/gpt-4",
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .expect("request should deserialize");

        let err = crate::middleware::mapper::TryConvert::try_convert(&converter, request)
            .expect_err("expected catalog mapping when is_router is false");

        assert!(
            matches!(
                err,
                crate::error::mapper::MapperError::NoModelMapping(_, _)
                    | crate::error::mapper::MapperError::NoProviderConfig(_)
            ),
            "unexpected error: {err:?}"
        );
    }

    #[tokio::test]
    async fn openai_compatible_converter_does_not_reapply_reasoning_rules() {
        let mut config = crate::config::Config::default();
        let named_provider = InferenceProvider::Named("custom-openai".into());
        config
            .providers
            .insert(named_provider.clone(), named_provider_config());
        let app = crate::app::build_test_app(config).await.expect("build app");
        let model_mapper = ModelMapper::new(app.state.clone());
        let capabilities = ProviderCapabilities::for_provider(&named_provider);
        let rules = default_provider_rules(&named_provider);
        let converter = super::OpenAICompatibleConverter::new_with_metadata(
            named_provider,
            capabilities,
            rules,
            model_mapper,
        );
        let mut request = sample_request();
        request.reasoning_effort = Some(async_openai::types::ReasoningEffort::High);

        let converted = crate::middleware::mapper::TryConvert::try_convert(&converter, request)
            .expect("conversion should succeed");

        assert_eq!(
            converted.inner.reasoning_effort,
            Some(async_openai::types::ReasoningEffort::High)
        );
    }

    #[tokio::test]
    async fn openai_compatible_converter_does_not_reapply_capability_gated_request_rules() {
        let mut config = crate::config::Config::default();
        let provider = InferenceProvider::Named("custom-openai".into());
        config
            .providers
            .insert(provider.clone(), named_provider_config());
        let app = crate::app::build_test_app(config).await.expect("build app");
        let model_mapper = ModelMapper::new(app.state.clone());
        let capabilities = ProviderCapabilities {
            provider: provider.clone(),
            openai_compatible: true,
            supports_response_format: false,
            supports_tool_choice: true,
            supports_parallel_tool_calls: false,
            supports_thinking: false,
            supports_streaming_reasoning: false,
        };
        let rules = default_provider_rules(&provider);
        let converter = super::OpenAICompatibleConverter::new_with_metadata(
            provider,
            capabilities,
            rules,
            model_mapper,
        );
        let request: CreateChatCompletionRequest = serde_json::from_value(json!({
            "model": "custom-openai/gpt-4o-mini",
            "messages": [
                {
                    "role": "user",
                    "content": "hello"
                }
            ],
            "parallel_tool_calls": true,
            "response_format": {
                "type": "json_object"
            }
        }))
        .expect("request should deserialize");

        let converted = crate::middleware::mapper::TryConvert::try_convert(&converter, request)
            .expect("conversion should succeed");

        assert_eq!(converted.inner.parallel_tool_calls, Some(true));
        assert_eq!(
            converted
                .inner
                .response_format
                .as_ref()
                .and_then(|value| serde_json::to_value(value).ok())
                .and_then(|value| value.get("type").cloned()),
            Some(json!("json_object"))
        );
    }

    #[tokio::test]
    async fn openai_compatible_converter_does_not_reapply_tool_rules() {
        let mut config = crate::config::Config::default();
        let provider = InferenceProvider::Named("custom-openai".into());
        config
            .providers
            .insert(provider.clone(), named_provider_config());
        let app = crate::app::build_test_app(config).await.expect("build app");
        let model_mapper = ModelMapper::new(app.state.clone());
        let capabilities = ProviderCapabilities::for_provider(&provider);
        let mut rules = default_provider_rules(&provider);
        rules.request.tool_choice_mode =
            crate::middleware::mapper::rules::ToolChoiceMode::Unsupported;
        let converter = super::OpenAICompatibleConverter::new_with_metadata(
            provider,
            capabilities,
            rules,
            model_mapper,
        );
        let request: CreateChatCompletionRequest = serde_json::from_value(json!({
            "model": "custom-openai/gpt-4o-mini",
            "messages": [
                {
                    "role": "user",
                    "content": "hello"
                }
            ],
            "tools": [
                {
                    "type": "function",
                    "function": {
                        "name": "weather",
                        "description": "lookup weather",
                        "parameters": {
                            "type": "object",
                            "properties": {
                                "city": {
                                    "type": "string"
                                }
                            },
                            "required": ["city"]
                        }
                    }
                }
            ],
            "tool_choice": "auto",
            "parallel_tool_calls": true
        }))
        .expect("request should deserialize");

        let converted = crate::middleware::mapper::TryConvert::try_convert(&converter, request)
            .expect("conversion should succeed");

        assert!(converted.inner.tools.is_some());
        assert_eq!(
            converted.inner.tool_choice,
            Some(async_openai::types::ChatCompletionToolChoiceOption::Auto)
        );
        assert_eq!(converted.inner.parallel_tool_calls, Some(true));
    }

    #[tokio::test]
    async fn openai_compatible_converter_does_not_reject_multimodal_without_request_engine() {
        let mut config = crate::config::Config::default();
        let provider = InferenceProvider::Named("custom-openai".into());
        config
            .providers
            .insert(provider.clone(), named_provider_config());
        let app = crate::app::build_test_app(config).await.expect("build app");
        let model_mapper = ModelMapper::new(app.state.clone());
        let capabilities = ProviderCapabilities::for_provider(&provider);
        let mut rules = default_provider_rules(&provider);
        rules.request.multimodal_mode =
            crate::middleware::mapper::rules::MultimodalMode::Unsupported;
        let converter = super::OpenAICompatibleConverter::new_with_metadata(
            provider,
            capabilities,
            rules,
            model_mapper,
        );

        let converted = crate::middleware::mapper::TryConvert::try_convert(
            &converter,
            sample_multimodal_request(),
        )
        .expect("conversion should succeed");

        assert_eq!(converted.inner.messages.len(), 1);
    }

    #[test]
    fn openai_compatible_converter_rejects_non_openai_family_metadata() {
        let provider = InferenceProvider::Named("custom-openai".into());
        let capabilities = ProviderCapabilities {
            provider: provider.clone(),
            openai_compatible: false,
            supports_response_format: false,
            supports_tool_choice: false,
            supports_parallel_tool_calls: false,
            supports_thinking: false,
            supports_streaming_reasoning: false,
        };
        let mut rules: ProviderRuleSet = default_provider_rules(&provider);
        rules.family =
            crate::middleware::mapper::families::ProviderProtocolFamily::AnthropicMessages;
        let mut config = crate::config::Config::default();
        config
            .providers
            .insert(provider.clone(), named_provider_config());
        let app = tokio::runtime::Runtime::new()
            .expect("runtime")
            .block_on(crate::app::build_test_app(config))
            .expect("build app");
        let model_mapper = ModelMapper::new(app.state.clone());

        let err = super::OpenAICompatibleConverter::try_new_with_metadata(
            provider,
            capabilities,
            rules,
            model_mapper,
        )
        .expect_err("metadata mismatch should fail");

        assert!(matches!(
            err,
            crate::error::mapper::MapperError::ProviderNotSupported(_)
        ));
    }

    #[tokio::test]
    async fn openai_compatible_converter_does_not_reapply_non_stream_profile_to_response_format() {
        let mut config = crate::config::Config::default();
        let provider = InferenceProvider::Named("custom-openai".into());
        config
            .providers
            .insert(provider.clone(), named_provider_config());
        let app = crate::app::build_test_app(config).await.expect("build app");
        let model_mapper = ModelMapper::new(app.state.clone());
        let capabilities = ProviderCapabilities::for_provider(&provider);
        let rules = default_provider_rules(&provider);
        let mut non_stream_profile =
            crate::middleware::mapper::non_stream_profile_data::default_non_stream_profile(
                &provider,
            );
        non_stream_profile.request.response_format_mode =
            crate::middleware::mapper::rules::ResponseFormatMode::Unsupported;
        let converter = super::OpenAICompatibleConverter::new_with_profile_metadata(
            provider,
            capabilities,
            rules,
            non_stream_profile,
            model_mapper,
        );
        let request: CreateChatCompletionRequest = serde_json::from_value(json!({
            "model": "custom-openai/gpt-4o-mini",
            "messages": [
                {
                    "role": "user",
                    "content": "hello"
                }
            ],
            "response_format": {
                "type": "json_object"
            }
        }))
        .expect("request should deserialize");

        let converted = crate::middleware::mapper::TryConvert::try_convert(&converter, request)
            .expect("conversion should succeed");

        assert_eq!(
            converted
                .inner
                .response_format
                .as_ref()
                .and_then(|value| serde_json::to_value(value).ok())
                .and_then(|value| value.get("type").cloned()),
            Some(json!("json_object"))
        );
    }

    #[tokio::test]
    async fn openai_compatible_converter_does_not_reapply_non_stream_profile_to_tool_fields() {
        let mut config = crate::config::Config::default();
        let provider = InferenceProvider::Named("custom-openai".into());
        config
            .providers
            .insert(provider.clone(), named_provider_config());
        let app = crate::app::build_test_app(config).await.expect("build app");
        let model_mapper = ModelMapper::new(app.state.clone());
        let capabilities = ProviderCapabilities::for_provider(&provider);
        let mut rules = default_provider_rules(&provider);
        rules.request.tool_choice_mode = crate::middleware::mapper::rules::ToolChoiceMode::Native;
        let mut non_stream_profile =
            crate::middleware::mapper::non_stream_profile_data::default_non_stream_profile(
                &provider,
            );
        non_stream_profile.request.tool_choice_mode =
            crate::middleware::mapper::rules::ToolChoiceMode::Unsupported;
        let converter = super::OpenAICompatibleConverter::new_with_profile_metadata(
            provider,
            capabilities,
            rules,
            non_stream_profile,
            model_mapper,
        );
        let request: CreateChatCompletionRequest = serde_json::from_value(json!({
            "model": "custom-openai/gpt-4o-mini",
            "messages": [
                {
                    "role": "user",
                    "content": "hello"
                }
            ],
            "tools": [
                {
                    "type": "function",
                    "function": {
                        "name": "weather",
                        "description": "lookup weather",
                        "parameters": {
                            "type": "object",
                            "properties": {
                                "city": {
                                    "type": "string"
                                }
                            },
                            "required": ["city"]
                        }
                    }
                }
            ],
            "tool_choice": "auto",
            "parallel_tool_calls": true
        }))
        .expect("request should deserialize");

        let converted = crate::middleware::mapper::TryConvert::try_convert(&converter, request)
            .expect("conversion should succeed");

        assert!(converted.inner.tools.is_some());
        assert_eq!(
            converted.inner.tool_choice,
            Some(async_openai::types::ChatCompletionToolChoiceOption::Auto)
        );
        assert_eq!(converted.inner.parallel_tool_calls, Some(true));
    }

    #[tokio::test]
    async fn openai_compatible_converter_does_not_reapply_non_stream_profile_to_reasoning_effort() {
        let mut config = crate::config::Config::default();
        let provider = InferenceProvider::Named("custom-openai".into());
        config
            .providers
            .insert(provider.clone(), named_provider_config());
        let app = crate::app::build_test_app(config).await.expect("build app");
        let model_mapper = ModelMapper::new(app.state.clone());
        let capabilities = ProviderCapabilities::for_provider(&provider);
        let mut rules = default_provider_rules(&provider);
        rules.request.reasoning_mode = crate::middleware::mapper::rules::ReasoningMode::Passthrough;
        let mut non_stream_profile =
            crate::middleware::mapper::non_stream_profile_data::default_non_stream_profile(
                &provider,
            );
        non_stream_profile.request.reasoning_mode =
            crate::middleware::mapper::rules::ReasoningMode::Unsupported;
        let converter = super::OpenAICompatibleConverter::new_with_profile_metadata(
            provider,
            capabilities,
            rules,
            non_stream_profile,
            model_mapper,
        );
        let mut request = sample_request();
        request.reasoning_effort = Some(async_openai::types::ReasoningEffort::High);

        let converted = crate::middleware::mapper::TryConvert::try_convert(&converter, request)
            .expect("conversion should succeed");

        assert_eq!(
            converted.inner.reasoning_effort,
            Some(async_openai::types::ReasoningEffort::High)
        );
    }

    #[tokio::test]
    async fn openai_compatible_converter_does_not_reject_multimodal_for_non_stream_profile() {
        let mut config = crate::config::Config::default();
        let provider = InferenceProvider::Named("custom-openai".into());
        config
            .providers
            .insert(provider.clone(), named_provider_config());
        let app = crate::app::build_test_app(config).await.expect("build app");
        let model_mapper = ModelMapper::new(app.state.clone());
        let capabilities = ProviderCapabilities::for_provider(&provider);
        let mut rules = default_provider_rules(&provider);
        rules.request.multimodal_mode =
            crate::middleware::mapper::rules::MultimodalMode::OpenAiStyle;
        let mut non_stream_profile =
            crate::middleware::mapper::non_stream_profile_data::default_non_stream_profile(
                &provider,
            );
        non_stream_profile.request.multimodal_mode =
            crate::middleware::mapper::rules::MultimodalMode::Unsupported;
        let converter = super::OpenAICompatibleConverter::new_with_profile_metadata(
            provider,
            capabilities,
            rules,
            non_stream_profile,
            model_mapper,
        );

        let converted = crate::middleware::mapper::TryConvert::try_convert(
            &converter,
            sample_multimodal_request(),
        )
        .expect("conversion should succeed");

        assert_eq!(converted.inner.messages.len(), 1);
    }

    #[tokio::test]
    async fn openai_compatible_converter_responses_request_catalog_mapping_rewrites_model() {
        let mut config = crate::config::Config::default();
        let named_provider = InferenceProvider::Named("custom-openai".into());
        config
            .providers
            .insert(named_provider.clone(), named_provider_config());
        let app = crate::app::build_test_app(config).await.expect("build app");
        let model_mapper = ModelMapper::new(app.state.clone());
        let capabilities = ProviderCapabilities::for_provider(&named_provider);
        let rules = default_provider_rules(&named_provider);
        let converter = super::OpenAICompatibleConverter::new_with_metadata(
            named_provider.clone(),
            capabilities,
            rules,
            model_mapper,
        );

        let converted = crate::middleware::mapper::TryConvert::try_convert(
            &converter,
            sample_responses_request(),
        )
        .expect("conversion should succeed");

        assert_eq!(converted.model, "gpt-4o-mini");
    }

    #[tokio::test]
    async fn openai_compatible_converter_responses_request_skips_catalog_preserves_raw_model() {
        let config = crate::config::Config::default();
        let app = crate::app::build_test_app(config).await.expect("build app");
        let mut flags = FxHashMap::default();
        flags.insert("openai".to_string(), false);
        flags.insert("openrouter".to_string(), true);
        app.state.set_provider_is_router_flags(flags);
        let openrouter = InferenceProvider::Named("openrouter".into());
        let model_mapper = ModelMapper::new(app.state.clone());
        let capabilities = ProviderCapabilities::for_provider(&openrouter);
        let rules = default_provider_rules(&openrouter);
        let converter = super::OpenAICompatibleConverter::new_with_metadata(
            openrouter.clone(),
            capabilities,
            rules,
            model_mapper,
        );
        let request = async_openai::types::responses::CreateResponse {
            model: "openrouter/anthropic/claude-3-haiku".to_string(),
            input: async_openai::types::responses::Input::Text("hello".to_string()),
            ..Default::default()
        };

        let converted = crate::middleware::mapper::TryConvert::try_convert(&converter, request)
            .expect("conversion should succeed");

        assert_eq!(converted.model, "openrouter/anthropic/claude-3-haiku");
    }

    #[tokio::test]
    async fn openai_compatible_converter_embeddings_request_catalog_mapping_rewrites_model() {
        let mut config = crate::config::Config::default();
        let named_provider = InferenceProvider::Named("custom-openai".into());
        config
            .providers
            .insert(named_provider.clone(), named_provider_config());
        let app = crate::app::build_test_app(config).await.expect("build app");
        let model_mapper = ModelMapper::new(app.state.clone());
        let capabilities = ProviderCapabilities::for_provider(&named_provider);
        let rules = default_provider_rules(&named_provider);
        let converter = super::OpenAICompatibleConverter::new_with_metadata(
            named_provider.clone(),
            capabilities,
            rules,
            model_mapper,
        );

        let converted = crate::middleware::mapper::TryConvert::try_convert(
            &converter,
            sample_embedding_request(),
        )
        .expect("conversion should succeed");

        assert_eq!(converted.model, "text-embedding-3-small");
    }

    #[tokio::test]
    async fn openai_compatible_converter_embeddings_request_skips_catalog_preserves_raw_model() {
        let config = crate::config::Config::default();
        let app = crate::app::build_test_app(config).await.expect("build app");
        let mut flags = FxHashMap::default();
        flags.insert("openai".to_string(), false);
        flags.insert("openrouter".to_string(), true);
        app.state.set_provider_is_router_flags(flags);
        let openrouter = InferenceProvider::Named("openrouter".into());
        let model_mapper = ModelMapper::new(app.state.clone());
        let capabilities = ProviderCapabilities::for_provider(&openrouter);
        let rules = default_provider_rules(&openrouter);
        let converter = super::OpenAICompatibleConverter::new_with_metadata(
            openrouter.clone(),
            capabilities,
            rules,
            model_mapper,
        );
        let request: CreateEmbeddingRequest = serde_json::from_value(json!({
            "model": "openrouter/openai/text-embedding-3-small",
            "input": "hello"
        }))
        .expect("embedding request should deserialize");

        let converted = crate::middleware::mapper::TryConvert::try_convert(&converter, request)
            .expect("conversion should succeed");

        assert_eq!(converted.model, "openrouter/openai/text-embedding-3-small");
    }

    #[tokio::test]
    async fn openrouter_skips_model_parsing_for_bare_responses_model() {
        let config = crate::config::Config::default();
        let app = crate::app::build_test_app(config).await.expect("build app");
        let mut flags = FxHashMap::default();
        flags.insert("openrouter".to_string(), true);
        app.state.set_provider_is_router_flags(flags);
        let openrouter = InferenceProvider::Named("openrouter".into());
        let model_mapper = ModelMapper::new(app.state.clone());
        let capabilities = ProviderCapabilities::for_provider(&openrouter);
        let rules = default_provider_rules(&openrouter);
        let converter = super::OpenAICompatibleConverter::new_with_metadata(
            openrouter,
            capabilities,
            rules,
            model_mapper,
        );
        let request: async_openai::types::responses::CreateResponse =
            serde_json::from_value(json!({
                "model": "gpt-5.4",
                "input": "hello"
            }))
            .expect("request should deserialize");

        let converted = crate::middleware::mapper::TryConvert::try_convert(&converter, request)
            .expect("conversion should succeed");

        assert_eq!(converted.model, "gpt-5.4");
    }

    #[tokio::test]
    async fn openrouter_responses_preserves_function_tool_choice_type() {
        let config = crate::config::Config::default();
        let app = crate::app::build_test_app(config).await.expect("build app");
        let mut flags = FxHashMap::default();
        flags.insert("openrouter".to_string(), true);
        app.state.set_provider_is_router_flags(flags);

        let openrouter = InferenceProvider::Named("openrouter".into());
        let model_mapper = ModelMapper::new(app.state.clone());
        let capabilities = ProviderCapabilities::for_provider(&openrouter);
        let rules = default_provider_rules(&openrouter);
        let converter = super::OpenAICompatibleConverter::new_with_metadata(
            openrouter,
            capabilities,
            rules,
            model_mapper,
        );

        let converted = convert_responses_body(
            converter,
            json!({
                "model": "openai/o4-mini",
                "input": "Use the lookup_ticket tool.",
                "tools": [{
                    "type": "function",
                    "name": "lookup_ticket",
                    "description": "Lookup a support ticket",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "ticket_id": { "type": "string" }
                        },
                        "required": ["ticket_id"]
                    }
                }],
                "tool_choice": {
                    "type": "function",
                    "name": "lookup_ticket"
                }
            }),
        );

        assert_eq!(converted["tool_choice"]["type"], "function");
        assert_eq!(converted["tool_choice"]["name"], "lookup_ticket");
    }

    #[tokio::test]
    async fn openrouter_responses_keeps_tool_choice_auto_mode() {
        let config = crate::config::Config::default();
        let app = crate::app::build_test_app(config).await.expect("build app");
        let mut flags = FxHashMap::default();
        flags.insert("openrouter".to_string(), true);
        app.state.set_provider_is_router_flags(flags);

        let openrouter = InferenceProvider::Named("openrouter".into());
        let model_mapper = ModelMapper::new(app.state.clone());
        let capabilities = ProviderCapabilities::for_provider(&openrouter);
        let rules = default_provider_rules(&openrouter);
        let converter = super::OpenAICompatibleConverter::new_with_metadata(
            openrouter,
            capabilities,
            rules,
            model_mapper,
        );

        let converted = convert_responses_body(
            converter,
            json!({
                "model": "openai/o4-mini",
                "input": "hello",
                "tools": [{
                    "type": "function",
                    "name": "lookup_ticket",
                    "parameters": { "type": "object" }
                }],
                "tool_choice": "auto"
            }),
        );

        assert_eq!(converted["tool_choice"], "auto");
    }

    #[tokio::test]
    async fn openrouter_responses_keeps_tool_choice_none_mode() {
        let config = crate::config::Config::default();
        let app = crate::app::build_test_app(config).await.expect("build app");
        let mut flags = FxHashMap::default();
        flags.insert("openrouter".to_string(), true);
        app.state.set_provider_is_router_flags(flags);

        let openrouter = InferenceProvider::Named("openrouter".into());
        let model_mapper = ModelMapper::new(app.state.clone());
        let capabilities = ProviderCapabilities::for_provider(&openrouter);
        let rules = default_provider_rules(&openrouter);
        let converter = super::OpenAICompatibleConverter::new_with_metadata(
            openrouter,
            capabilities,
            rules,
            model_mapper,
        );

        let converted = convert_responses_body(
            converter,
            json!({
                "model": "openai/o4-mini",
                "input": "hello",
                "tools": [{
                    "type": "function",
                    "name": "lookup_ticket",
                    "parameters": { "type": "object" }
                }],
                "tool_choice": "none"
            }),
        );

        assert_eq!(converted["tool_choice"], "none");
    }

    #[tokio::test]
    async fn openrouter_responses_keeps_tool_choice_required_mode() {
        let config = crate::config::Config::default();
        let app = crate::app::build_test_app(config).await.expect("build app");
        let mut flags = FxHashMap::default();
        flags.insert("openrouter".to_string(), true);
        app.state.set_provider_is_router_flags(flags);

        let openrouter = InferenceProvider::Named("openrouter".into());
        let model_mapper = ModelMapper::new(app.state.clone());
        let capabilities = ProviderCapabilities::for_provider(&openrouter);
        let rules = default_provider_rules(&openrouter);
        let converter = super::OpenAICompatibleConverter::new_with_metadata(
            openrouter,
            capabilities,
            rules,
            model_mapper,
        );

        let converted = convert_responses_body(
            converter,
            json!({
                "model": "openai/o4-mini",
                "input": "hello",
                "tools": [{
                    "type": "function",
                    "name": "lookup_ticket",
                    "parameters": { "type": "object" }
                }],
                "tool_choice": "required"
            }),
        );

        assert_eq!(converted["tool_choice"], "required");
    }

    #[tokio::test]
    async fn openrouter_responses_without_tool_choice_stays_unchanged() {
        let config = crate::config::Config::default();
        let app = crate::app::build_test_app(config).await.expect("build app");
        let mut flags = FxHashMap::default();
        flags.insert("openrouter".to_string(), true);
        app.state.set_provider_is_router_flags(flags);

        let openrouter = InferenceProvider::Named("openrouter".into());
        let model_mapper = ModelMapper::new(app.state.clone());
        let capabilities = ProviderCapabilities::for_provider(&openrouter);
        let rules = default_provider_rules(&openrouter);
        let converter = super::OpenAICompatibleConverter::new_with_metadata(
            openrouter,
            capabilities,
            rules,
            model_mapper,
        );

        let converted = convert_responses_body(
            converter,
            json!({
                "model": "openai/o4-mini",
                "input": "hello",
                "tools": [{
                    "type": "function",
                    "name": "lookup_ticket",
                    "parameters": { "type": "object" }
                }]
            }),
        );

        assert!(converted.get("tool_choice").is_none());
    }

    #[tokio::test]
    async fn non_openrouter_responses_does_not_apply_tool_choice_patch() {
        let mut config = crate::config::Config::default();
        let provider = InferenceProvider::Named("custom-openai".into());
        config
            .providers
            .insert(provider.clone(), named_provider_config());
        let app = crate::app::build_test_app(config).await.expect("build app");
        let model_mapper = ModelMapper::new(app.state.clone());
        let capabilities = ProviderCapabilities::for_provider(&provider);
        let rules = default_provider_rules(&provider);
        let converter = super::OpenAICompatibleConverter::new_with_metadata(
            provider,
            capabilities,
            rules,
            model_mapper,
        );

        let converted = convert_responses_body(
            converter,
            json!({
                "model": "custom-openai/gpt-4o-mini",
                "input": "Use the lookup_ticket tool.",
                "tools": [{
                    "type": "function",
                    "name": "lookup_ticket",
                    "parameters": { "type": "object" }
                }],
                "tool_choice": {
                    "type": "function",
                    "name": "lookup_ticket"
                }
            }),
        );

        assert!(converted["tool_choice"].get("type").is_none());
        assert_eq!(converted["tool_choice"]["name"], "lookup_ticket");
    }

    #[tokio::test]
    async fn openrouter_responses_does_not_patch_unknown_function_tool_choice() {
        let config = crate::config::Config::default();
        let app = crate::app::build_test_app(config).await.expect("build app");
        let mut flags = FxHashMap::default();
        flags.insert("openrouter".to_string(), true);
        app.state.set_provider_is_router_flags(flags);

        let openrouter = InferenceProvider::Named("openrouter".into());
        let model_mapper = ModelMapper::new(app.state.clone());
        let capabilities = ProviderCapabilities::for_provider(&openrouter);
        let rules = default_provider_rules(&openrouter);
        let converter = super::OpenAICompatibleConverter::new_with_metadata(
            openrouter,
            capabilities,
            rules,
            model_mapper,
        );

        let converted = convert_responses_body(
            converter,
            json!({
                "model": "openai/o4-mini",
                "input": "Use a tool.",
                "tools": [{
                    "type": "function",
                    "name": "lookup_customer",
                    "parameters": { "type": "object" }
                }],
                "tool_choice": {
                    "type": "function",
                    "name": "lookup_ticket"
                }
            }),
        );

        assert!(converted["tool_choice"].get("type").is_none());
        assert_eq!(converted["tool_choice"]["name"], "lookup_ticket");
    }

    #[tokio::test]
    async fn openai_compatible_injects_stream_options_when_streaming() {
        let mut config = crate::config::Config::default();
        let named_provider = InferenceProvider::Named("custom-openai".into());
        config
            .providers
            .insert(named_provider.clone(), named_provider_config());
        let app = crate::app::build_test_app(config).await.expect("build app");
        let model_mapper = ModelMapper::new(app.state.clone());
        let capabilities = ProviderCapabilities::for_provider(&named_provider);
        let rules = default_provider_rules(&named_provider);
        let converter = super::OpenAICompatibleConverter::new_with_metadata(
            named_provider,
            capabilities,
            rules,
            model_mapper,
        );
        let mut request = sample_request();
        request.stream = Some(true);

        let converted = crate::middleware::mapper::TryConvert::try_convert(&converter, request)
            .expect("conversion should succeed");

        let stream_options = converted
            .inner
            .stream_options
            .expect("stream_options should be injected");
        assert!(stream_options.include_usage);
    }

    #[tokio::test]
    async fn openai_compatible_preserves_existing_stream_options() {
        let mut config = crate::config::Config::default();
        let named_provider = InferenceProvider::Named("custom-openai".into());
        config
            .providers
            .insert(named_provider.clone(), named_provider_config());
        let app = crate::app::build_test_app(config).await.expect("build app");
        let model_mapper = ModelMapper::new(app.state.clone());
        let capabilities = ProviderCapabilities::for_provider(&named_provider);
        let rules = default_provider_rules(&named_provider);
        let converter = super::OpenAICompatibleConverter::new_with_metadata(
            named_provider,
            capabilities,
            rules,
            model_mapper,
        );
        let mut request = sample_request();
        request.stream = Some(true);
        request.stream_options = Some(async_openai::types::ChatCompletionStreamOptions {
            include_usage: false,
        });

        let converted = crate::middleware::mapper::TryConvert::try_convert(&converter, request)
            .expect("conversion should succeed");

        let stream_options = converted
            .inner
            .stream_options
            .expect("stream_options should be preserved");
        assert!(
            !stream_options.include_usage,
            "existing stream_options should not be overwritten"
        );
    }

    #[tokio::test]
    async fn openai_compatible_no_stream_options_when_not_streaming() {
        let mut config = crate::config::Config::default();
        let named_provider = InferenceProvider::Named("custom-openai".into());
        config
            .providers
            .insert(named_provider.clone(), named_provider_config());
        let app = crate::app::build_test_app(config).await.expect("build app");
        let model_mapper = ModelMapper::new(app.state.clone());
        let capabilities = ProviderCapabilities::for_provider(&named_provider);
        let rules = default_provider_rules(&named_provider);
        let converter = super::OpenAICompatibleConverter::new_with_metadata(
            named_provider,
            capabilities,
            rules,
            model_mapper,
        );
        let request = sample_request();

        let converted = crate::middleware::mapper::TryConvert::try_convert(&converter, request)
            .expect("conversion should succeed");

        assert!(
            converted.inner.stream_options.is_none(),
            "stream_options should remain None when not streaming"
        );
    }
}
