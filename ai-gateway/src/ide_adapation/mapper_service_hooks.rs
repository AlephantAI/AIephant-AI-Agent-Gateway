//! Orchestration in mapper `map_request` / `map_response` for client profiles,
//! Cursor Responses,\ and IDE ingress preprocessing (extracted from
//! `middleware::mapper::service`).

use std::sync::{Arc, Mutex};

use bytes::{BufMut, Bytes, BytesMut};
use futures::TryStreamExt;
use http::request::Parts as RequestParts;
use opentelemetry::KeyValue;

use crate::{
    app_state::AppState,
    endpoints::{ApiEndpoint, anthropic::Anthropic, openai::OpenAI},
    error::{
        api::ApiError, internal::InternalError, invalid_req::InvalidRequestError,
        mapper::MapperError, stream::StreamError,
    },
    ide_adapation::{
        client_profile::{
            ClientProfile, ClientProfileResolution, native_semantic_passthrough,
            resolve_client_profile,
        },
        cursor_responses_openrouter_bridge,
        ide_ingress_adjust::{IdeIngressAdjustMeta, apply_ide_ingress_adjust},
        unified_responses_chat_compat::{
            BridgeStreamState, non_stream_responses_body_to_chat_completion,
        },
    },
    middleware::mapper::registry::EndpointConverterRegistry,
    types::{
        extensions::{
            ClientResponseSemantic, LoggerResponseWireSemantic, MapperContext,
            NativeSemanticPassthrough,
        },
        model_id::ModelId,
        response::Response,
    },
};

#[must_use]
pub(crate) fn resolve_and_attach_client_profile(
    parts: &mut RequestParts,
) -> ClientProfileResolution {
    let client_profile_resolution = resolve_client_profile(&parts.headers);
    parts.extensions.insert(client_profile_resolution.clone());
    client_profile_resolution
}

/// After VK policy: adjust IDE ingress body and record `ide_ingress_adjust`
/// trace / counter.
pub(crate) fn apply_ide_ingress_adjust_record_metrics(
    app_state: &AppState,
    profile: ClientProfile,
    source_endpoint: &ApiEndpoint,
    extensions: &http::Extensions,
    body: Bytes,
) -> Result<(Bytes, IdeIngressAdjustMeta), ApiError> {
    let (body, ide_ingress_adjust_meta) =
        apply_ide_ingress_adjust(profile, source_endpoint, body, extensions)?;
    tracing::trace!(
        client_profile = profile.as_otel_label(),
        ide_ingress_adjust_applied = ide_ingress_adjust_meta.applied,
        source_endpoint = ?source_endpoint,
        "ide_ingress_adjust"
    );
    app_state.0.metrics.mapper_ide_ingress_adjust_total.add(
        1,
        &[
            KeyValue::new("profile", ide_ingress_adjust_meta.profile_label),
            KeyValue::new(
                "applied",
                if ide_ingress_adjust_meta.applied {
                    "true"
                } else {
                    "false"
                },
            ),
        ],
    );
    Ok((body, ide_ingress_adjust_meta))
}

/// Records client profile resolution and native semantic passthrough decision;
/// inserts [`NativeSemanticPassthrough`] extension when enabled.
#[must_use]
pub(crate) fn record_client_profile_passthrough_metrics_and_extension(
    app_state: &AppState,
    parts: &mut RequestParts,
    client_profile_resolution: &ClientProfileResolution,
    source_endpoint: &ApiEndpoint,
    target_endpoint: &ApiEndpoint,
) -> bool {
    let passthrough = native_semantic_passthrough(
        client_profile_resolution.profile,
        source_endpoint,
        target_endpoint,
        &target_endpoint.provider(),
    );
    app_state.0.metrics.mapper_client_profile_resolved.add(
        1,
        &[
            KeyValue::new("profile", client_profile_resolution.profile.as_otel_label()),
            KeyValue::new(
                "explicit",
                if client_profile_resolution.from_explicit_header {
                    "true"
                } else {
                    "false"
                },
            ),
        ],
    );
    app_state.0.metrics.mapper_native_semantic_passthrough.add(
        1,
        &[KeyValue::new(
            "passthrough",
            if passthrough { "true" } else { "false" },
        )],
    );
    if passthrough {
        parts.extensions.insert(NativeSemanticPassthrough);
    }
    passthrough
}

/// Builds [`MapperContext`] on native semantic passthrough paths (e.g. Claude
/// Code / Codex); skips semantic envelope when source/target share the same
/// wire family.
pub(crate) fn mapper_context_native_semantic_passthrough(
    source_endpoint: &ApiEndpoint,
    target_endpoint: &ApiEndpoint,
    body: &Bytes,
) -> Result<MapperContext, ApiError> {
    use anthropic_ai_sdk::types::message::CreateMessageParams;
    use async_openai::types::CreateChatCompletionRequest;

    let provider = target_endpoint.provider();
    match source_endpoint {
        ApiEndpoint::OpenAI(OpenAI::ChatCompletions(_)) => {
            let req = serde_json::from_slice::<CreateChatCompletionRequest>(body)
                .map_err(InvalidRequestError::InvalidRequestBody)?;
            let is_stream = req.stream.unwrap_or(false);
            let model = ModelId::from_str_and_provider(provider, &req.model)
                .map_err(InternalError::MapperError)?;
            let anthropic_openai_usage = is_stream.then(|| {
                std::sync::Arc::new(std::sync::Mutex::new(
                    crate::types::extensions::AnthropicStreamOpenAiUsageState::default(),
                ))
            });
            Ok(MapperContext {
                is_stream,
                client_response_semantic: ClientResponseSemantic::ChatCompletions,
                logger_response_wire_semantic: if is_stream {
                    LoggerResponseWireSemantic::ChatCompletionsSse
                } else {
                    LoggerResponseWireSemantic::ChatCompletionsJson
                },
                model: Some(model),
                anthropic_openai_usage,
                unified_responses_bridge_chat_completions_sse: false,
                native_semantic_passthrough: true,
                cursor_responses_via_chat_completions: false,
                cursor_responses_origin: None,
                client_expects_responses_wire: false,
            })
        }
        ApiEndpoint::OpenAI(OpenAI::Responses(_)) => {
            let fields =
                crate::ide_adapation::responses_ingress_normalize::responses_request_routing_fields(
                    body,
                )?;
            let model = ModelId::from_str_and_provider(provider, &fields.model)
                .map_err(InternalError::MapperError)?;
            Ok(MapperContext {
                is_stream: fields.stream,
                client_response_semantic: ClientResponseSemantic::Responses,
                logger_response_wire_semantic: if fields.stream {
                    LoggerResponseWireSemantic::ResponsesSse
                } else {
                    LoggerResponseWireSemantic::ResponsesJson
                },
                model: Some(model),
                anthropic_openai_usage: None,
                unified_responses_bridge_chat_completions_sse: false,
                native_semantic_passthrough: true,
                cursor_responses_via_chat_completions: false,
                cursor_responses_origin: None,
                client_expects_responses_wire: false,
            })
        }
        ApiEndpoint::Anthropic(Anthropic::Messages(_)) => {
            let r = serde_json::from_slice::<CreateMessageParams>(body)
                .map_err(InvalidRequestError::InvalidRequestBody)?;
            let is_stream = r.stream.unwrap_or(false);
            let model = ModelId::from_str_and_provider(provider, &r.model)
                .map_err(InternalError::MapperError)?;
            let anthropic_openai_usage = is_stream.then(|| {
                std::sync::Arc::new(std::sync::Mutex::new(
                    crate::types::extensions::AnthropicStreamOpenAiUsageState::default(),
                ))
            });
            Ok(MapperContext {
                is_stream,
                client_response_semantic: ClientResponseSemantic::Other,
                logger_response_wire_semantic: LoggerResponseWireSemantic::Other,
                model: Some(model),
                anthropic_openai_usage,
                unified_responses_bridge_chat_completions_sse: false,
                native_semantic_passthrough: true,
                cursor_responses_via_chat_completions: false,
                cursor_responses_origin: None,
                client_expects_responses_wire: false,
            })
        }
        _ => Err(ApiError::Internal(InternalError::MapperError(
            MapperError::InvalidRequest,
        ))),
    }
}

/// Cursor `/v1/responses` → Chat Completions request branch; returns a triple
/// on match.
pub(crate) fn try_cursor_responses_compatible_chat_mapping(
    converter_registry: &EndpointConverterRegistry,
    profile: ClientProfile,
    source_endpoint: &ApiEndpoint,
    target_endpoint: &ApiEndpoint,
    body: &Bytes,
    unified_model_body_passthrough: bool,
) -> Result<Option<(Bytes, MapperContext, ApiEndpoint)>, ApiError> {
    cursor_responses_openrouter_bridge::try_map_responses_to_compatible_chat(
        converter_registry,
        profile,
        source_endpoint,
        target_endpoint,
        body,
        unified_model_body_passthrough,
    )
}

/// Outcome of the Cursor Responses branch in `map_response`: final response
/// produced, or continue with generic mapper response handling.
pub(crate) enum CursorResponsesMapOutcome {
    Done(Response),
    Continue {
        parts: http::response::Parts,
        body: crate::types::body::Body,
    },
}

/// Response rewrite / SSE passthrough when Cursor Responses goes upstream via
/// Chat Completions in `map_response`. Returns
/// [`CursorResponsesMapOutcome::Continue`] when not applicable.
pub(crate) async fn map_response_cursor_responses_branch(
    mapper_ctx: &MapperContext,
    unified_responses_bridge_chat_completions_sse: bool,
    parts: http::response::Parts,
    body: crate::types::body::Body,
) -> Result<CursorResponsesMapOutcome, ApiError> {
    let is_stream = mapper_ctx.is_stream;
    if !mapper_ctx.cursor_responses_via_chat_completions {
        return Ok(CursorResponsesMapOutcome::Continue { parts, body });
    }

    if is_stream {
        if unified_responses_bridge_chat_completions_sse
            && !mapper_ctx.client_expects_responses_wire
        {
            tracing::trace!(
                "cursor + unified chat URL: passthrough upstream Chat \
                 Completions SSE (no Responses-shape rewrite)"
            );
            let mapped_stream = body
                .into_data_stream()
                .map_err(|e| ApiError::StreamError(StreamError::BodyError(e)))
                .map_ok(|bytes| {
                    let mut new_bytes = BytesMut::new();
                    new_bytes.put("data: ".as_bytes());
                    new_bytes.put(bytes);
                    new_bytes.put("\n\n".as_bytes());
                    new_bytes.freeze()
                });
            let final_body = axum_core::body::Body::new(reqwest::Body::wrap_stream(mapped_stream));
            return Ok(CursorResponsesMapOutcome::Done(Response::from_parts(
                parts, final_body,
            )));
        }
        tracing::trace!(
            "cursor responses → OpenAI-compatible chat/completions SSE; map \
             Chat SSE back to Responses"
        );
        let origin = mapper_ctx
            .cursor_responses_origin
            .clone()
            .ok_or(InternalError::ExtensionNotFound("cursor_responses_origin"))?;
        let resp = cursor_responses_openrouter_bridge::map_stream_response_chat_to_responses(
            parts,
            body,
            origin,
            mapper_ctx.client_expects_responses_wire,
        )
        .await?;
        return Ok(CursorResponsesMapOutcome::Done(resp));
    }

    if unified_responses_bridge_chat_completions_sse && !mapper_ctx.client_expects_responses_wire {
        tracing::trace!(
            "cursor + unified chat URL: passthrough upstream Chat Completions \
             JSON (no Responses-shape rewrite)"
        );
        return Ok(CursorResponsesMapOutcome::Done(Response::from_parts(
            parts, body,
        )));
    }
    tracing::trace!(
        "cursor responses → OpenAI-compatible chat/completions JSON; map Chat \
         JSON back to Responses"
    );
    let origin = mapper_ctx
        .cursor_responses_origin
        .as_ref()
        .ok_or(InternalError::ExtensionNotFound("cursor_responses_origin"))?;
    let resp = cursor_responses_openrouter_bridge::map_json_response_chat_to_responses(
        parts, body, origin,
    )
    .await?;
    Ok(CursorResponsesMapOutcome::Done(resp))
}

/// Outcome of the unified Responses → Chat Completions response bridge in
/// `map_response`: final response produced, or continue with generic mapper
/// response handling.
pub(crate) enum UnifiedResponsesChatBridgeMapOutcome {
    Done(Response),
    Continue {
        parts: http::response::Parts,
        body: crate::types::body::Body,
    },
}

/// Streaming Responses-SSE → Chat Completions-SSE bridge for unified
/// Chat Completions clients. Returns
/// [`UnifiedResponsesChatBridgeMapOutcome::Continue`] when not applicable.
pub(crate) fn map_stream_unified_responses_chat_bridge(
    bridge_chat_completions: bool,
    is_stream: bool,
    parts: http::response::Parts,
    body: crate::types::body::Body,
) -> Result<UnifiedResponsesChatBridgeMapOutcome, ApiError> {
    if !bridge_chat_completions || !is_stream {
        return Ok(UnifiedResponsesChatBridgeMapOutcome::Continue { parts, body });
    }

    tracing::trace!("unified responses → Chat Completions SSE bridge (streaming)");
    let state = Arc::new(Mutex::new(BridgeStreamState::default()));
    let mapped_stream = body
        .into_data_stream()
        .map_err(|e| ApiError::StreamError(StreamError::BodyError(e)))
        .try_filter_map(move |bytes| {
            let state = Arc::clone(&state);
            async move {
                let opt = {
                    let mut guard = state.lock().expect("responses bridge mutex poisoned");
                    guard.process_upstream_sse_json(&bytes)?
                };
                Ok(opt)
            }
        });
    let final_body = axum_core::body::Body::new(reqwest::Body::wrap_stream(mapped_stream));
    Ok(UnifiedResponsesChatBridgeMapOutcome::Done(
        Response::from_parts(parts, final_body),
    ))
}

/// Non-streaming Responses JSON → Chat Completions JSON bridge for unified
/// Chat Completions clients.
pub(crate) fn map_non_stream_unified_responses_chat_bridge(
    body_bytes: &Bytes,
) -> Result<Bytes, ApiError> {
    non_stream_responses_body_to_chat_completion(body_bytes)
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use serde_json::json;

    use super::*;
    use crate::types::extensions::LoggerResponseWireSemantic;

    #[test]
    fn native_semantic_passthrough_context_supports_responses() {
        let body = Bytes::from(
            serde_json::to_vec(&json!({
                "model": "gpt-4o-mini",
                "input": "hi",
                "stream": false
            }))
            .unwrap(),
        );
        let ctx = mapper_context_native_semantic_passthrough(
            &ApiEndpoint::OpenAI(OpenAI::responses()),
            &ApiEndpoint::OpenAI(OpenAI::responses()),
            &body,
        )
        .unwrap();

        assert!(!ctx.is_stream);
        assert!(ctx.native_semantic_passthrough);
        assert!(!ctx.cursor_responses_via_chat_completions);
        assert_eq!(
            ctx.client_response_semantic,
            ClientResponseSemantic::Responses
        );
    }

    #[test]
    fn native_semantic_passthrough_context_accepts_responses_priority_tier() {
        let body = Bytes::from(
            serde_json::to_vec(&json!({
                "model": "gpt-5.5",
                "input": "hi",
                "stream": true,
                "service_tier": "priority"
            }))
            .unwrap(),
        );
        let ctx = mapper_context_native_semantic_passthrough(
            &ApiEndpoint::OpenAI(OpenAI::responses()),
            &ApiEndpoint::OpenAI(OpenAI::responses()),
            &body,
        )
        .unwrap();

        assert!(ctx.is_stream);
        assert!(ctx.native_semantic_passthrough);
        assert_eq!(
            ctx.client_response_semantic,
            ClientResponseSemantic::Responses
        );
        assert_eq!(
            ctx.logger_response_wire_semantic,
            LoggerResponseWireSemantic::ResponsesSse
        );
    }

    #[test]
    fn native_semantic_passthrough_context_marks_stream_chat_completions() {
        let body = Bytes::from(
            serde_json::to_vec(&json!({
                "model": "gpt-4o-mini",
                "messages": [{ "role": "user", "content": "hi" }],
                "stream": true
            }))
            .unwrap(),
        );
        let ctx = mapper_context_native_semantic_passthrough(
            &ApiEndpoint::OpenAI(OpenAI::chat_completions()),
            &ApiEndpoint::OpenAI(OpenAI::chat_completions()),
            &body,
        )
        .unwrap();

        assert_eq!(
            ctx.client_response_semantic,
            ClientResponseSemantic::ChatCompletions
        );
        assert_eq!(
            ctx.logger_response_wire_semantic,
            LoggerResponseWireSemantic::ChatCompletionsSse
        );
    }
}
