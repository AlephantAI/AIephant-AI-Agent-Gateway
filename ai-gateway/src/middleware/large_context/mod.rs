pub mod headers;
pub mod heuristics;
pub mod input_token_estimate;
pub mod model_limits;
pub mod parse;

use bytes::Bytes;
use http::request::Parts;
use serde_json::Value;

use self::{
    headers::{TokenLimitExceptionHandler, parse_large_context_headers},
    heuristics::{
        apply_fallback, apply_middle_out, apply_truncate,
        compute_input_budget_tokens, estimate_input_tokens,
        extract_fallback_model_candidates, resolve_primary_model,
    },
    model_limits::{
        ModelContextLimitResolver, StaticModelContextLimitResolver,
    },
    parse::{parse_chat_completions_payload, serialize_payload},
};
use crate::{
    app_state::AppState,
    error::{api::ApiError, invalid_req::InvalidRequestError},
    types::extensions::{LargeContextAction, LargeContextDecision},
};

fn with_model(raw: &Value, model: &str) -> Value {
    let mut cloned = raw.clone();
    if let Some(object) = cloned.as_object_mut() {
        object.insert("model".to_string(), Value::String(model.to_string()));
    }
    cloned
}

fn current_body_model(payload: &parse::ChatCompletionsPayload) -> Option<&str> {
    payload.model.as_deref()
}

pub fn maybe_transform_unified_api_chat_request(
    _app_state: &AppState,
    parts: &mut Parts,
    body: Bytes,
) -> Result<Bytes, ApiError> {
    let headers = parse_large_context_headers(&parts.headers)
        .map_err(ApiError::InvalidRequest)?;
    let Some(handler) = headers.handler else {
        return Ok(body);
    };

    let payload = match parse_chat_completions_payload(&body) {
        Ok(Some(payload)) => payload,
        Ok(None) | Err(_) => return Ok(body),
    };
    let model_source =
        current_body_model(&payload).or(headers.model_override.as_deref());
    let Some(primary_model) = resolve_primary_model(
        current_body_model(&payload),
        headers.model_override.as_deref(),
    ) else {
        let decision = LargeContextDecision {
            handler,
            action: LargeContextAction::SkippedNoModel,
            original_model: model_source.map(str::to_string),
            effective_model: None,
            estimated_input_tokens: None,
            model_context_limit: None,
            input_budget_tokens: None,
        };
        tracing::debug!(
            large_context_handler = decision.handler.as_str(),
            large_context_action = decision.action.as_str(),
            "large context decision recorded"
        );
        parts.extensions.insert(decision);
        return Ok(body);
    };

    let resolver = StaticModelContextLimitResolver;
    let model_context_limit = resolver.resolve(&primary_model);
    let estimated_input_tokens =
        estimate_input_tokens(&payload, &primary_model, None);
    let input_budget_tokens = model_context_limit.map(|limit| {
        compute_input_budget_tokens(limit, payload.requested_completion_tokens)
    });
    let normalized_model_body = (current_body_model(&payload)
        != Some(primary_model.as_str()))
    .then(|| with_model(&payload.raw, &primary_model));

    let (action, transformed_body) = match handler {
        TokenLimitExceptionHandler::Fallback => {
            let transformed = model_source.and_then(|model_source| {
                apply_fallback(
                    &payload,
                    model_source,
                    estimated_input_tokens,
                    input_budget_tokens,
                )
            });
            let action = if transformed.is_some() {
                LargeContextAction::FallbackApplied
            } else if model_source.is_some_and(|model_source| {
                extract_fallback_model_candidates(model_source).len() < 2
            }) {
                LargeContextAction::SkippedNoFallbackModel
            } else {
                LargeContextAction::SkippedBelowLimit
            };
            (action, transformed.or(normalized_model_body))
        }
        TokenLimitExceptionHandler::Truncate => {
            if payload.messages.is_empty() {
                (
                    LargeContextAction::SkippedNoTextMessages,
                    normalized_model_body,
                )
            } else if payload.has_non_text_message_content {
                (
                    LargeContextAction::SkippedNonTextMessages,
                    normalized_model_body,
                )
            } else if model_context_limit.is_none() {
                (
                    LargeContextAction::SkippedNoModelLimit,
                    normalized_model_body,
                )
            } else if estimated_input_tokens.is_none() {
                (LargeContextAction::SkippedNoEstimate, normalized_model_body)
            } else if estimated_input_tokens
                .zip(input_budget_tokens)
                .is_some_and(|(estimated, budget)| estimated < budget)
            {
                (LargeContextAction::SkippedBelowLimit, normalized_model_body)
            } else {
                let transformed = apply_truncate(
                    &payload,
                    &primary_model,
                    input_budget_tokens.expect("budget should exist"),
                );
                (
                    transformed.as_ref().map_or(
                        LargeContextAction::SkippedNoTextMessages,
                        |_| LargeContextAction::Truncated,
                    ),
                    transformed.or(normalized_model_body),
                )
            }
        }
        TokenLimitExceptionHandler::MiddleOut => {
            if payload.messages.is_empty() {
                (
                    LargeContextAction::SkippedNoTextMessages,
                    normalized_model_body,
                )
            } else if payload.has_non_text_message_content {
                (
                    LargeContextAction::SkippedNonTextMessages,
                    normalized_model_body,
                )
            } else if model_context_limit.is_none() {
                (
                    LargeContextAction::SkippedNoModelLimit,
                    normalized_model_body,
                )
            } else if estimated_input_tokens.is_none() {
                (LargeContextAction::SkippedNoEstimate, normalized_model_body)
            } else if estimated_input_tokens
                .zip(input_budget_tokens)
                .is_some_and(|(estimated, budget)| estimated < budget)
            {
                (LargeContextAction::SkippedBelowLimit, normalized_model_body)
            } else {
                let transformed = apply_middle_out(
                    &payload,
                    &primary_model,
                    input_budget_tokens.expect("budget should exist"),
                );
                (
                    transformed.as_ref().map_or(
                        LargeContextAction::SkippedNoTextMessages,
                        |_| LargeContextAction::MiddleOutApplied,
                    ),
                    transformed.or(normalized_model_body),
                )
            }
        }
    };

    let effective_model = transformed_body
        .as_ref()
        .and_then(|value| value.get("model"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| Some(primary_model.clone()));
    let decision = LargeContextDecision {
        handler,
        action,
        original_model: model_source.map(str::to_string),
        effective_model,
        estimated_input_tokens,
        model_context_limit,
        input_budget_tokens,
    };
    tracing::debug!(
        large_context_handler = decision.handler.as_str(),
        large_context_action = decision.action.as_str(),
        large_context_original_model = ?decision.original_model,
        large_context_effective_model = ?decision.effective_model,
        large_context_estimated_tokens = ?decision.estimated_input_tokens,
        large_context_input_budget_tokens = ?decision.input_budget_tokens,
        "large context decision recorded"
    );
    parts.extensions.insert(decision);

    if let Some(transformed_body) = transformed_body {
        serialize_payload(&transformed_body).map_err(|error| {
            ApiError::InvalidRequest(InvalidRequestError::InvalidRequestBody(
                error,
            ))
        })
    } else {
        Ok(body)
    }
}
