//! Unified API target_provider resolution.

use std::str::FromStr;

use crate::{
    error::{api::ApiError, internal::InternalError, invalid_req::InvalidRequestError},
    types::{model_id::ModelId, provider::InferenceProvider},
};

/// Route using `ModelId::from_str` on a bare model name (e.g. `org/model`
/// style).
#[allow(dead_code)]
pub(crate) fn provider_from_bare_model(model: &str) -> Result<InferenceProvider, ApiError> {
    let source_model = ModelId::from_str(model).map_err(InternalError::MapperError)?;
    match source_model {
        ModelId::ModelIdWithVersion { provider, .. } => Ok(provider),
        ModelId::Bedrock(_) => Ok(InferenceProvider::Bedrock),
        ModelId::Ollama(_) => Ok(InferenceProvider::Ollama),
        ModelId::Unknown(_) => Err(InvalidRequestError::UnsupportedEndpoint(format!(
            "provider for the given model: '{source_model}' not supported"
        ))
        .into()),
    }
}

/// Resolve the unified-API target `InferenceProvider`, taking into account
/// the master-key-bound allow-list (if any).
///
/// Semantics:
/// - No allow-list (or empty): preserve original behavior — return
///   `compat_fallback` in compat mode, otherwise infer from the model.
/// - Single-provider allow-list: pin to that provider (compat does not override
///   the pin).
/// - Multi-provider allow-list: resolve via compat fallback or model inference,
///   then verify the resolved provider is in the allow-list; reject with
///   `UnsupportedGatewayModel` otherwise.
#[allow(dead_code)]
pub(crate) fn resolve_unified_target_provider(
    compat: bool,
    compat_fallback: InferenceProvider,
    model: &str,
    master_key_allowed_providers: Option<&[InferenceProvider]>,
) -> Result<InferenceProvider, ApiError> {
    let allowed = master_key_allowed_providers.filter(|s| !s.is_empty());

    if let Some(list) = allowed {
        if list.len() == 1 {
            return Ok(list[0].clone());
        }
        let resolved = if compat {
            compat_fallback
        } else {
            provider_from_bare_model(model)?
        };
        if list.contains(&resolved) {
            Ok(resolved)
        } else {
            Err(InvalidRequestError::UnsupportedGatewayModel(
                "requested model routes to a provider not allowed for this \
                 API key"
                    .to_string(),
            )
            .into())
        }
    } else if compat {
        Ok(compat_fallback)
    } else {
        provider_from_bare_model(model)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowed_none_compat_false() {
        let provider = resolve_unified_target_provider(
            false,
            InferenceProvider::OpenAI,
            "openai/gpt-4o-mini",
            None,
        )
        .expect("resolve should succeed");
        assert_eq!(provider, InferenceProvider::OpenAI);
    }

    #[test]
    fn allowed_single_pins_despite_model_pointing_elsewhere() {
        let list = vec![InferenceProvider::Anthropic];
        let provider = resolve_unified_target_provider(
            false,
            InferenceProvider::OpenAI,
            "openai/gpt-4o-mini",
            Some(list.as_slice()),
        )
        .expect("resolve should succeed");
        assert_eq!(provider, InferenceProvider::Anthropic);
    }

    #[test]
    fn allowed_single_pins_despite_compat() {
        let list = vec![InferenceProvider::Anthropic];
        let provider = resolve_unified_target_provider(
            true,
            InferenceProvider::OpenAI,
            "anything/whatever",
            Some(list.as_slice()),
        )
        .expect("resolve should succeed");
        assert_eq!(provider, InferenceProvider::Anthropic);
    }

    #[test]
    fn allowed_single_router_provider_accepts_bare_model() {
        let list = vec![InferenceProvider::Named("openrouter".into())];
        let provider = resolve_unified_target_provider(
            false,
            InferenceProvider::OpenAI,
            "gpt-5.4",
            Some(list.as_slice()),
        )
        .expect("single allowed provider should pin routing");

        assert_eq!(provider, InferenceProvider::Named("openrouter".into()));
    }

    #[test]
    fn allowed_multi_happy() {
        let list = vec![InferenceProvider::OpenAI, InferenceProvider::Anthropic];
        let provider = resolve_unified_target_provider(
            false,
            InferenceProvider::OpenAI,
            "openai/gpt-4o-mini",
            Some(list.as_slice()),
        )
        .expect("resolve should succeed");
        assert_eq!(provider, InferenceProvider::OpenAI);
    }

    /// Multi-provider allow-list excludes the model-resolved provider →
    /// expect `UnsupportedGatewayModel`. The plan's original 1-element
    /// `[Anthropic]` list would short-circuit via the pin branch and never
    /// exercise the reject path; we use a 2-element list (`[Anthropic,
    /// Bedrock]`) without `OpenAI` so the reject branch is actually hit.
    #[test]
    fn allowed_multi_rejects() {
        let list = vec![InferenceProvider::Anthropic, InferenceProvider::Bedrock];
        let err = resolve_unified_target_provider(
            false,
            InferenceProvider::OpenAI,
            "openai/x",
            Some(list.as_slice()),
        )
        .expect_err("resolve should reject");
        match err {
            ApiError::InvalidRequest(InvalidRequestError::UnsupportedGatewayModel(_)) => {}
            other => panic!("expected UnsupportedGatewayModel, got {other:?}"),
        }
    }

    #[test]
    fn allowed_multi_provider_still_rejects_bare_model_without_provider_prefix() {
        let list = vec![
            InferenceProvider::Named("openrouter".into()),
            InferenceProvider::OpenAI,
        ];
        let err = resolve_unified_target_provider(
            false,
            InferenceProvider::OpenAI,
            "gpt-5.4",
            Some(list.as_slice()),
        )
        .expect_err("bare model should not be inferred for multi-provider keys");

        match err {
            ApiError::Internal(InternalError::MapperError(_))
            | ApiError::InvalidRequest(InvalidRequestError::UnsupportedGatewayModel(_)) => {}
            other => panic!("unexpected error: {other:?}"),
        }
    }

    /// Compat fallback (`OpenAI`) not in multi-provider allow-list. As
    /// above, the spec's 1-element list would pin and skip the reject
    /// branch; we use `[Anthropic, Bedrock]` so the reject path runs.
    #[test]
    fn allowed_multi_compat_not_in_list() {
        let list = vec![InferenceProvider::Anthropic, InferenceProvider::Bedrock];
        let err = resolve_unified_target_provider(
            true,
            InferenceProvider::OpenAI,
            "openai/x",
            Some(list.as_slice()),
        )
        .expect_err("resolve should reject");
        match err {
            ApiError::InvalidRequest(InvalidRequestError::UnsupportedGatewayModel(_)) => {}
            other => panic!("expected UnsupportedGatewayModel, got {other:?}"),
        }
    }

    #[test]
    fn allowed_empty_slice_treated_as_none() {
        let empty: [InferenceProvider; 0] = [];
        let provider = resolve_unified_target_provider(
            false,
            InferenceProvider::OpenAI,
            "openai/gpt-4o-mini",
            Some(empty.as_slice()),
        )
        .expect("resolve should succeed");
        assert_eq!(provider, InferenceProvider::OpenAI);
    }

    #[test]
    fn allowed_none_compat_true_uses_fallback() {
        let provider = resolve_unified_target_provider(
            true,
            InferenceProvider::Anthropic,
            "openai/gpt-4o-mini",
            None,
        )
        .expect("resolve should succeed");
        assert_eq!(provider, InferenceProvider::Anthropic);
    }
}
