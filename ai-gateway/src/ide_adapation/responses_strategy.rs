use bytes::Bytes;

use crate::{
    endpoints::{ApiEndpoint, openai::OpenAI},
    error::{api::ApiError, internal::InternalError},
    ide_adapation::{
        client_profile::ClientProfile,
        responses_ingress_normalize::{
            apply_openai_responses_wire_normalize_for_client,
            apply_responses_wire_normalize_for_client,
        },
    },
};

pub(crate) fn normalize_responses_for_target(
    source_endpoint: &ApiEndpoint,
    target_endpoint: &ApiEndpoint,
    body: Bytes,
    profile: ClientProfile,
) -> Result<Bytes, ApiError> {
    if !matches!(source_endpoint, ApiEndpoint::OpenAI(OpenAI::Responses(_))) {
        return Ok(body);
    }

    if matches!(
        target_endpoint,
        ApiEndpoint::OpenAICompatible {
            openai_endpoint: OpenAI::Responses(_),
            ..
        }
    ) {
        apply_responses_wire_normalize_for_client(body, profile)
    } else {
        apply_openai_responses_wire_normalize_for_client(body, profile)
    }
}

pub(crate) fn maybe_preconvert_responses_to_chat(
    source_endpoint: ApiEndpoint,
    target_endpoint: &ApiEndpoint,
    body: Bytes,
) -> Result<(Bytes, ApiEndpoint), ApiError> {
    let should_preconvert = matches!(&source_endpoint, ApiEndpoint::OpenAI(OpenAI::Responses(_)))
        && !matches!(
            target_endpoint,
            ApiEndpoint::OpenAI(OpenAI::Responses(_))
                | ApiEndpoint::OpenAICompatible {
                    openai_endpoint: OpenAI::Responses(_),
                    ..
                }
        );

    if !should_preconvert {
        return Ok((body, source_endpoint));
    }

    let create_response: async_openai::types::responses::CreateResponse =
        serde_json::from_slice(&body).map_err(|error| InternalError::Deserialize {
            ty: "CreateResponse",
            error,
        })?;
    let chat_request =
        crate::middleware::mapper::responses_to_chat_request::convert(create_response)
            .map_err(InternalError::MapperError)?;
    let new_body = Bytes::from(serde_json::to_vec(&chat_request).map_err(|error| {
        InternalError::Serialize {
            ty: "CreateChatCompletionRequest",
            error,
        }
    })?);
    tracing::debug!(
        "mapper: pre-converted Responses request to ChatCompletions for \
         cross-protocol target"
    );
    Ok((new_body, ApiEndpoint::OpenAI(OpenAI::chat_completions())))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        endpoints::{ApiEndpoint, openai::OpenAI},
        ide_adapation::client_profile::ClientProfile,
        types::provider::InferenceProvider,
    };

    #[test]
    fn normalize_responses_accepts_priority_service_tier_for_openai_target() {
        let body = Bytes::from(
            serde_json::to_vec(&json!({
                "model": "gpt-5.5",
                "input": "hi",
                "stream": true,
                "service_tier": "priority"
            }))
            .unwrap(),
        );
        let source = ApiEndpoint::OpenAI(OpenAI::responses());
        let target = ApiEndpoint::OpenAI(OpenAI::responses());

        let out = normalize_responses_for_target(&source, &target, body, ClientProfile::Unknown)
            .expect("normalize should accept routing fields");

        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["model"], "gpt-5.5");
        assert_eq!(v["stream"], true);
    }

    #[test]
    fn normalize_responses_for_openai_compatible_uses_compat_wire() {
        let body = Bytes::from(
            serde_json::to_vec(&json!({
                "model": "gpt-5.5",
                "input": "hi",
                "tools": [{
                    "type": "tool_search",
                    "description": "Search deferred tools",
                    "execution": "client"
                }]
            }))
            .unwrap(),
        );
        let source = ApiEndpoint::OpenAI(OpenAI::responses());
        let target = ApiEndpoint::OpenAICompatible {
            provider: InferenceProvider::Named("openrouter".into()),
            openai_endpoint: OpenAI::responses(),
        };

        let out = normalize_responses_for_target(&source, &target, body, ClientProfile::CodexCli)
            .expect("normalize should rewrite Codex tool_search");

        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["tools"][0]["type"], "function");
        assert_eq!(v["tools"][0]["name"], "tool_search");
        assert!(v["tools"][0].get("execution").is_none());
    }

    #[test]
    fn preconvert_responses_to_chat_for_anthropic_target() {
        let body = Bytes::from(
            serde_json::to_vec(&json!({
                "model": "gpt-5.5",
                "input": "hello"
            }))
            .unwrap(),
        );
        let source = ApiEndpoint::OpenAI(OpenAI::responses());
        let target = ApiEndpoint::Anthropic(crate::endpoints::anthropic::Anthropic::messages());

        let (out, new_source) = maybe_preconvert_responses_to_chat(source, &target, body)
            .expect("preconvert should succeed");

        assert!(matches!(
            new_source,
            ApiEndpoint::OpenAI(OpenAI::ChatCompletions(_))
        ));
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["messages"][0]["role"], "user");
    }
}
