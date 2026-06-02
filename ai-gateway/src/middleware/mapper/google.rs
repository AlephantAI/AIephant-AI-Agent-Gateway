use http::response::Parts;

use super::{
    TryConvertStreamData, capabilities::ProviderCapabilities,
    families::ProviderProtocolFamily, model::ModelMapper,
    non_stream_profile::NonStreamFormatProfile,
    non_stream_profile_data::default_non_stream_profile,
    params::OpenAiRequestParams, rules::ProviderRuleSet,
};
use crate::{
    endpoints::openai::OpenAICompatibleChatCompletionRequest,
    error::mapper::MapperError,
    middleware::mapper::{ResponseBodyConverter, TryConvert, TryConvertError},
    types::provider::InferenceProvider,
};

pub struct GoogleConverter {
    non_stream_profile: NonStreamFormatProfile,
    model_mapper: ModelMapper,
}

impl GoogleConverter {
    fn derived_profile_from_metadata(
        provider: &InferenceProvider,
        rules: &ProviderRuleSet,
    ) -> NonStreamFormatProfile {
        let mut non_stream_profile = default_non_stream_profile(provider);
        non_stream_profile.family = rules.family;
        non_stream_profile.request.system_handling =
            rules.request.system_handling;
        non_stream_profile.request.tool_choice_mode =
            rules.request.tool_choice_mode;
        non_stream_profile.request.response_format_mode =
            rules.request.response_format_mode;
        non_stream_profile.request.reasoning_mode =
            rules.request.reasoning_mode;
        non_stream_profile.request.multimodal_mode =
            rules.request.multimodal_mode;
        non_stream_profile
    }

    #[allow(dead_code)]
    #[must_use]
    pub fn new(model_mapper: ModelMapper) -> Self {
        let capabilities = ProviderCapabilities::for_provider(
            &InferenceProvider::GoogleGemini,
        );
        let rules = super::rule_data::default_provider_rules(
            &InferenceProvider::GoogleGemini,
        );
        Self::new_with_metadata(capabilities, rules, model_mapper)
    }

    #[must_use]
    pub fn new_with_profile(
        non_stream_profile: NonStreamFormatProfile,
        model_mapper: ModelMapper,
    ) -> Self {
        Self {
            non_stream_profile,
            model_mapper,
        }
    }

    #[must_use]
    pub fn new_with_metadata(
        capabilities: ProviderCapabilities,
        rules: ProviderRuleSet,
        model_mapper: ModelMapper,
    ) -> Self {
        Self::try_new_with_metadata(capabilities, rules, model_mapper)
            .expect("google converter metadata must be valid")
    }

    pub fn try_new_with_metadata(
        capabilities: ProviderCapabilities,
        rules: ProviderRuleSet,
        model_mapper: ModelMapper,
    ) -> Result<Self, MapperError> {
        if capabilities.provider != InferenceProvider::GoogleGemini
            || rules.provider != InferenceProvider::GoogleGemini
            || rules.family != ProviderProtocolFamily::GeminiOpenAiLike
            || !rules.family.matches_provider(&capabilities.provider)
        {
            return Err(MapperError::ProviderNotSupported(
                capabilities.provider.to_string(),
            ));
        }

        let non_stream_profile =
            Self::derived_profile_from_metadata(&capabilities.provider, &rules);
        Ok(Self::new_with_profile(non_stream_profile, model_mapper))
    }
}

impl
    TryConvert<
        async_openai::types::CreateChatCompletionRequest,
        OpenAICompatibleChatCompletionRequest,
    > for GoogleConverter
{
    type Error = MapperError;

    fn try_convert(
        &self,
        mut value: async_openai::types::CreateChatCompletionRequest,
    ) -> Result<OpenAICompatibleChatCompletionRequest, Self::Error> {
        let source_model = OpenAiRequestParams::from_request(&value)
            .source_model
            .ok_or_else(|| {
                MapperError::InvalidModelName(value.model.clone())
            })?;
        let target_model = self
            .model_mapper
            .map_model(&source_model, &InferenceProvider::GoogleGemini)?;
        tracing::trace!(source_model = ?source_model, target_model = ?target_model, "mapped gemini model");
        value.model = target_model.to_string();

        Ok(OpenAICompatibleChatCompletionRequest {
            provider: InferenceProvider::GoogleGemini,
            inner: value,
        })
    }

    fn try_convert_model_passthrough(
        &self,
        value: async_openai::types::CreateChatCompletionRequest,
    ) -> Result<OpenAICompatibleChatCompletionRequest, Self::Error> {
        Ok(OpenAICompatibleChatCompletionRequest {
            provider: InferenceProvider::GoogleGemini,
            inner: value,
        })
    }
}

impl
    TryConvert<
        async_openai::types::CreateChatCompletionResponse,
        async_openai::types::CreateChatCompletionResponse,
    > for GoogleConverter
{
    type Error = MapperError;

    fn try_convert(
        &self,
        value: async_openai::types::CreateChatCompletionResponse,
    ) -> Result<async_openai::types::CreateChatCompletionResponse, Self::Error>
    {
        Ok(value)
    }
}

impl
    ResponseBodyConverter<
        async_openai::types::CreateChatCompletionResponse,
        async_openai::types::CreateChatCompletionResponse,
    > for GoogleConverter
{
    fn try_convert_response(
        &self,
        resp_parts: &Parts,
        value: async_openai::types::CreateChatCompletionResponse,
    ) -> Result<async_openai::types::CreateChatCompletionResponse, Self::Error>
    {
        super::non_stream_response_interpreter::apply_non_stream_response_profile_from_parts(
            resp_parts,
            &self.non_stream_profile,
            value,
        )
    }
}

impl
    TryConvertStreamData<
        async_openai::types::CreateChatCompletionStreamResponse,
        async_openai::types::CreateChatCompletionStreamResponse,
    > for GoogleConverter
{
    type Error = MapperError;

    fn try_convert_chunk(
        &self,
        value: async_openai::types::CreateChatCompletionStreamResponse,
        _anthropic_openai_usage: Option<
            &crate::types::extensions::AnthropicOpenAiUsageCell,
        >,
    ) -> Result<
        Option<async_openai::types::CreateChatCompletionStreamResponse>,
        Self::Error,
    > {
        Ok(Some(value))
    }
}

impl
    TryConvertError<
        async_openai::error::WrappedError,
        async_openai::error::WrappedError,
    > for GoogleConverter
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

#[cfg(test)]
mod tests {
    use async_openai::types::CreateChatCompletionRequest;
    use serde_json::json;

    use crate::{
        middleware::mapper::{
            capabilities::ProviderCapabilities,
            model::ModelMapper,
            non_stream_profile_data::default_non_stream_profile,
            rule_data::default_provider_rules,
            rules::{
                MultimodalMode, ReasoningMode, ResponseFormatMode,
                ToolChoiceMode,
            },
        },
        types::provider::InferenceProvider,
    };

    fn sample_request() -> CreateChatCompletionRequest {
        serde_json::from_value(json!({
            "model": "gemini/gemini-2.0-flash",
            "messages": [
                {
                    "role": "user",
                    "content": "hello"
                }
            ]
        }))
        .expect("request should deserialize")
    }

    fn sample_multimodal_request() -> CreateChatCompletionRequest {
        serde_json::from_value(json!({
            "model": "gemini/gemini-2.0-flash",
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

    async fn sample_model_mapper() -> ModelMapper {
        let config = crate::config::Config::default();
        let app = crate::app::build_test_app(config).await.expect("build app");
        ModelMapper::new(app.state.clone())
    }

    #[tokio::test]
    async fn google_converter_does_not_reapply_non_stream_profile_to_tool_fields()
     {
        let mut non_stream_profile =
            default_non_stream_profile(&InferenceProvider::GoogleGemini);
        non_stream_profile.request.tool_choice_mode =
            ToolChoiceMode::Unsupported;
        let converter = super::GoogleConverter::new_with_profile(
            non_stream_profile,
            sample_model_mapper().await,
        );
        let request: CreateChatCompletionRequest =
            serde_json::from_value(json!({
                "model": "gemini/gemini-2.0-flash",
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
                            "name": "lookup_weather",
                            "description": "lookup weather"
                        }
                    }
                ],
                "tool_choice": "auto",
                "parallel_tool_calls": true
            }))
            .expect("request should deserialize");

        let converted = crate::middleware::mapper::TryConvert::try_convert(
            &converter, request,
        )
        .expect("conversion should succeed");

        assert!(converted.inner.tools.is_some());
        assert_eq!(
            converted.inner.tool_choice,
            Some(async_openai::types::ChatCompletionToolChoiceOption::Auto)
        );
        assert_eq!(converted.inner.parallel_tool_calls, Some(true));
    }

    #[tokio::test]
    async fn google_converter_does_not_reapply_non_stream_profile_to_reasoning_effort()
     {
        let mut non_stream_profile =
            default_non_stream_profile(&InferenceProvider::GoogleGemini);
        non_stream_profile.request.reasoning_mode = ReasoningMode::Unsupported;
        let converter = super::GoogleConverter::new_with_profile(
            non_stream_profile,
            sample_model_mapper().await,
        );
        let mut request = sample_request();
        request.reasoning_effort =
            Some(async_openai::types::ReasoningEffort::High);

        let converted = crate::middleware::mapper::TryConvert::try_convert(
            &converter, request,
        )
        .expect("conversion should succeed");

        assert_eq!(
            converted.inner.reasoning_effort,
            Some(async_openai::types::ReasoningEffort::High)
        );
    }

    #[tokio::test]
    async fn google_converter_does_not_reject_multimodal_without_request_engine()
     {
        let mut non_stream_profile =
            default_non_stream_profile(&InferenceProvider::GoogleGemini);
        non_stream_profile.request.multimodal_mode =
            MultimodalMode::Unsupported;
        let converter = super::GoogleConverter::new_with_profile(
            non_stream_profile,
            sample_model_mapper().await,
        );

        let converted = crate::middleware::mapper::TryConvert::try_convert(
            &converter,
            sample_multimodal_request(),
        )
        .expect("conversion should succeed");

        assert_eq!(converted.inner.messages.len(), 1);
    }

    #[tokio::test]
    async fn google_converter_does_not_reapply_non_stream_profile_to_response_format()
     {
        let mut non_stream_profile =
            default_non_stream_profile(&InferenceProvider::GoogleGemini);
        non_stream_profile.request.response_format_mode =
            ResponseFormatMode::Unsupported;
        let converter = super::GoogleConverter::new_with_profile(
            non_stream_profile,
            sample_model_mapper().await,
        );
        let request: CreateChatCompletionRequest =
            serde_json::from_value(json!({
                "model": "gemini/gemini-2.0-flash",
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

        let converted = crate::middleware::mapper::TryConvert::try_convert(
            &converter, request,
        )
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
    async fn google_converter_can_be_built_from_metadata_rules_without_reapplying_them()
     {
        let capabilities = ProviderCapabilities::for_provider(
            &InferenceProvider::GoogleGemini,
        );
        let mut rules =
            default_provider_rules(&InferenceProvider::GoogleGemini);
        rules.request.tool_choice_mode = ToolChoiceMode::Unsupported;
        let converter = super::GoogleConverter::new_with_metadata(
            capabilities,
            rules,
            sample_model_mapper().await,
        );
        let request: CreateChatCompletionRequest =
            serde_json::from_value(json!({
                "model": "gemini/gemini-2.0-flash",
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
                            "name": "lookup_weather"
                        }
                    }
                ],
                "tool_choice": "auto"
            }))
            .expect("request should deserialize");

        let converted = crate::middleware::mapper::TryConvert::try_convert(
            &converter, request,
        )
        .expect("conversion should succeed");

        assert!(converted.inner.tools.is_some());
        assert_eq!(
            converted.inner.tool_choice,
            Some(async_openai::types::ChatCompletionToolChoiceOption::Auto)
        );
    }
}
