use super::{
    capabilities::ProviderCapabilities,
    envelope::RequestEnvelope,
    rules::{ProviderRuleSet, RequestRuleContext, ResponseFormatMode, ToolChoiceMode},
};
use crate::error::mapper::MapperError;

#[must_use]
pub fn build_request_rule_context(
    capabilities: &ProviderCapabilities,
    rules: &ProviderRuleSet,
) -> RequestRuleContext {
    RequestRuleContext {
        provider: rules.provider.clone(),
        family: rules.family,
        system_handling: rules.request.system_handling,
        tool_choice_mode: if capabilities.supports_tool_choice {
            rules.request.tool_choice_mode
        } else {
            ToolChoiceMode::Unsupported
        },
        response_format_mode: if capabilities.supports_response_format {
            rules.request.response_format_mode
        } else {
            ResponseFormatMode::Unsupported
        },
        reasoning_mode: rules.request.reasoning_mode,
        multimodal_mode: rules.request.multimodal_mode,
        supports_parallel_tool_calls: capabilities.supports_parallel_tool_calls,
    }
}

pub fn prepare_request_envelope(
    mut envelope: RequestEnvelope,
) -> Result<RequestEnvelope, MapperError> {
    let Some(resolved) = envelope.resolved_metadata.as_ref() else {
        return Ok(envelope);
    };

    let context = build_request_rule_context(&resolved.capabilities, &resolved.rules);
    if !envelope.is_stream {
        super::non_stream_request_interpreter::apply_non_stream_request_profile(
            &resolved.non_stream_profile,
            &mut envelope.openai_request,
        )?;

        if !context.supports_parallel_tool_calls {
            envelope.openai_request.parallel_tool_calls = None;
        }
    }

    Ok(envelope.with_request_rule_context(context))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::{
        endpoints::{ApiEndpoint, openai::OpenAI},
        middleware::mapper::{
            capabilities::ProviderCapabilities, envelope::RequestEnvelope,
            profile_resolver::resolve_mapper_metadata, rule_data::default_provider_rules,
        },
        types::provider::InferenceProvider,
    };

    #[test]
    fn request_rule_engine_downgrades_response_format_when_capability_disallows_it() {
        let provider = InferenceProvider::Named("custom-openai".into());
        let capabilities = ProviderCapabilities {
            provider: provider.clone(),
            openai_compatible: true,
            supports_response_format: false,
            supports_tool_choice: true,
            supports_parallel_tool_calls: true,
            supports_thinking: false,
            supports_streaming_reasoning: false,
        };
        let rules = default_provider_rules(&provider);

        let context = super::build_request_rule_context(&capabilities, &rules);

        assert_eq!(
            context.response_format_mode,
            crate::middleware::mapper::rules::ResponseFormatMode::Unsupported
        );
    }

    #[test]
    fn request_rule_engine_prepares_non_stream_envelope_with_context() {
        let provider = InferenceProvider::Named("qwen".into());
        let request: async_openai::types::CreateChatCompletionRequest =
            serde_json::from_value(json!({
                "model": "qwen/qwen3-32b",
                "messages": [
                    {
                        "role": "user",
                        "content": "hello"
                    }
                ],
                "reasoning_effort": "high"
            }))
            .expect("request should deserialize");
        let envelope = RequestEnvelope::from_openai_chat_request(
            ApiEndpoint::OpenAI(OpenAI::chat_completions()),
            provider.clone(),
            request,
        )
        .with_resolved_metadata(
            resolve_mapper_metadata(&provider, Some("qwen/qwen3-32b"))
                .expect("metadata should resolve"),
        );

        let prepared = super::prepare_request_envelope(envelope).expect("should prepare");

        assert!(prepared.request_rule_context.is_some());
        assert!(prepared.openai_request.reasoning_effort.is_none());
    }

    #[test]
    fn request_rule_engine_leaves_stream_request_body_unchanged() {
        let provider = InferenceProvider::Named("qwen".into());
        let request: async_openai::types::CreateChatCompletionRequest =
            serde_json::from_value(json!({
                "model": "qwen/qwen3-32b",
                "stream": true,
                "messages": [
                    {
                        "role": "user",
                        "content": "hello"
                    }
                ],
                "reasoning_effort": "high"
            }))
            .expect("request should deserialize");
        let envelope = RequestEnvelope::from_openai_chat_request(
            ApiEndpoint::OpenAI(OpenAI::chat_completions()),
            provider.clone(),
            request,
        )
        .with_resolved_metadata(
            resolve_mapper_metadata(&provider, Some("qwen/qwen3-32b"))
                .expect("metadata should resolve"),
        );

        let prepared = super::prepare_request_envelope(envelope).expect("should prepare");

        assert!(prepared.request_rule_context.is_some());
        assert_eq!(
            prepared.openai_request.reasoning_effort,
            Some(async_openai::types::ReasoningEffort::High)
        );
    }

    #[test]
    fn deepseek_chat_still_strips_reasoning_effort_after_prepare_request() {
        let provider = InferenceProvider::Named("deepseek".into());
        let request: async_openai::types::CreateChatCompletionRequest =
            serde_json::from_value(json!({
                "model": "deepseek/deepseek-chat",
                "messages": [
                    {
                        "role": "user",
                        "content": "hello"
                    }
                ],
                "reasoning_effort": "high"
            }))
            .expect("request should deserialize");
        let envelope = RequestEnvelope::from_openai_chat_request(
            ApiEndpoint::OpenAI(OpenAI::chat_completions()),
            provider.clone(),
            request,
        )
        .with_resolved_metadata(
            resolve_mapper_metadata(&provider, Some("deepseek/deepseek-chat"))
                .expect("metadata should resolve"),
        );

        let prepared = super::prepare_request_envelope(envelope).expect("should prepare");

        assert!(prepared.openai_request.reasoning_effort.is_none());
    }

    #[test]
    fn deepseek_reasoner_keeps_reasoning_effort_via_model_override() {
        let provider = InferenceProvider::Named("deepseek".into());
        let request: async_openai::types::CreateChatCompletionRequest =
            serde_json::from_value(json!({
                "model": "deepseek/deepseek-reasoner",
                "messages": [
                    {
                        "role": "user",
                        "content": "hello"
                    }
                ],
                "reasoning_effort": "high"
            }))
            .expect("request should deserialize");
        let envelope = RequestEnvelope::from_openai_chat_request(
            ApiEndpoint::OpenAI(OpenAI::chat_completions()),
            provider.clone(),
            request,
        )
        .with_resolved_metadata(
            resolve_mapper_metadata(&provider, Some("deepseek/deepseek-reasoner"))
                .expect("metadata should resolve"),
        );

        let prepared = super::prepare_request_envelope(envelope).expect("should prepare");

        assert_eq!(
            prepared.openai_request.reasoning_effort,
            Some(async_openai::types::ReasoningEffort::High)
        );
    }
}
