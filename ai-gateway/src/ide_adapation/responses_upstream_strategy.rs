use crate::{
    endpoints::{ApiEndpoint, openai::OpenAI},
    ide_adapation::client_profile::{
        ClientProfile, endpoints_same_wire_family,
    },
    types::provider::InferenceProvider,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponsesUpstreamStrategy {
    NativePassthrough,
    BridgeToChat,
    Convert,
}

pub fn responses_upstream_strategy(
    profile: ClientProfile,
    source: &ApiEndpoint,
    target: &ApiEndpoint,
    provider: &InferenceProvider,
) -> ResponsesUpstreamStrategy {
    if profile == ClientProfile::CodexCli
        && matches!(provider, InferenceProvider::OpenAI)
        && endpoints_same_wire_family(source, target)
        && matches!(source, ApiEndpoint::OpenAI(OpenAI::Responses(_)))
    {
        return ResponsesUpstreamStrategy::NativePassthrough;
    }

    let bridge =
        matches!(profile, ClientProfile::CodexCli | ClientProfile::CursorIde)
            && matches!(source, ApiEndpoint::OpenAI(OpenAI::Responses(_)))
            && matches!(
                target,
                ApiEndpoint::OpenAICompatible {
                    openai_endpoint: OpenAI::Responses(_),
                    ..
                }
            );
    if bridge {
        return ResponsesUpstreamStrategy::BridgeToChat;
    }

    ResponsesUpstreamStrategy::Convert
}

#[cfg(test)]
mod tests {
    use super::*;

    fn openrouter() -> InferenceProvider {
        InferenceProvider::from_provider_code("OpenRouter").unwrap()
    }

    #[test]
    fn codex_openai_responses_to_responses_is_native_passthrough() {
        assert_eq!(
            responses_upstream_strategy(
                ClientProfile::CodexCli,
                &ApiEndpoint::OpenAI(OpenAI::responses()),
                &ApiEndpoint::OpenAI(OpenAI::responses()),
                &InferenceProvider::OpenAI,
            ),
            ResponsesUpstreamStrategy::NativePassthrough
        );
    }

    #[test]
    fn codex_compatible_responses_bridges_to_chat() {
        let provider = openrouter();
        assert_eq!(
            responses_upstream_strategy(
                ClientProfile::CodexCli,
                &ApiEndpoint::OpenAI(OpenAI::responses()),
                &ApiEndpoint::OpenAICompatible {
                    provider: provider.clone(),
                    openai_endpoint: OpenAI::responses(),
                },
                &provider,
            ),
            ResponsesUpstreamStrategy::BridgeToChat
        );
    }

    #[test]
    fn cursor_openai_responses_uses_convert() {
        assert_eq!(
            responses_upstream_strategy(
                ClientProfile::CursorIde,
                &ApiEndpoint::OpenAI(OpenAI::responses()),
                &ApiEndpoint::OpenAI(OpenAI::responses()),
                &InferenceProvider::OpenAI,
            ),
            ResponsesUpstreamStrategy::Convert
        );
    }

    #[test]
    fn cursor_compatible_responses_bridges_to_chat() {
        let provider = openrouter();
        assert_eq!(
            responses_upstream_strategy(
                ClientProfile::CursorIde,
                &ApiEndpoint::OpenAI(OpenAI::responses()),
                &ApiEndpoint::OpenAICompatible {
                    provider: provider.clone(),
                    openai_endpoint: OpenAI::responses(),
                },
                &provider,
            ),
            ResponsesUpstreamStrategy::BridgeToChat
        );
    }
}
