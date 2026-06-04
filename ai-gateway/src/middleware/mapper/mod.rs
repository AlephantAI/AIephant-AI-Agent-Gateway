pub mod anthropic;
mod anthropic_error_lenient;
mod anthropic_stream_openai;
mod bedrock;
pub mod capabilities;
pub mod capability_data;
mod chat_completion_role_normalize;
pub mod envelope;
pub mod families;
pub mod google;
pub mod model;
pub mod non_stream_profile;
pub mod non_stream_profile_data;
pub mod non_stream_request_interpreter;
pub mod non_stream_response_interpreter;
pub mod ollama;
pub mod openai;
pub mod openai_compatible;
pub mod params;
pub mod profile_resolver;
pub mod registry;
pub mod request_rule_engine;
pub mod response_normalizer;
pub(crate) mod responses_to_chat_request;
pub mod rule_data;
pub mod rule_validator;
pub mod rules;
pub mod service;
pub mod stream_normalizer;
mod wrapped_error_lenient;

use async_openai::error::WrappedError;
use base64::Engine;
use bytes::Bytes;
use http::{StatusCode, response::Parts};
use serde::{Serialize, de::DeserializeOwned};

pub use self::service::*;
use crate::{
    endpoints::{AiRequest, Endpoint},
    error::{
        api::ApiError, internal::InternalError, invalid_req::InvalidRequestError,
        mapper::MapperError,
    },
    types::{
        extensions::{
            AnthropicOpenAiUsageCell, ClientResponseSemantic, LoggerResponseWireSemantic,
            MapperContext,
        },
        model_id::ModelId,
    },
};

pub(crate) const DEFAULT_MAX_TOKENS: u32 = 2000;

fn client_response_semantic_for_source_endpoint<S: Endpoint>() -> ClientResponseSemantic {
    match S::PATH {
        "v1/chat/completions" => ClientResponseSemantic::ChatCompletions,
        "v1/responses" => ClientResponseSemantic::Responses,
        _ => ClientResponseSemantic::Other,
    }
}

fn logger_response_wire_semantic_for_target_endpoint<T: Endpoint>(
    is_stream: bool,
) -> LoggerResponseWireSemantic {
    match (T::PATH, is_stream) {
        ("v1/chat/completions", true) => LoggerResponseWireSemantic::ChatCompletionsSse,
        ("v1/chat/completions", false) => LoggerResponseWireSemantic::ChatCompletionsJson,
        ("v1/responses", true) => LoggerResponseWireSemantic::ResponsesSse,
        ("v1/responses", false) => LoggerResponseWireSemantic::ResponsesJson,
        _ => LoggerResponseWireSemantic::Other,
    }
}

/// `TryFrom` but allows us to implement it for foreign types, so we can
/// maintain boundaries between our business logic and the provider types.
pub trait TryConvert<Source, Target>: Sized {
    type Error;

    fn try_convert(&self, value: Source) -> std::result::Result<Target, Self::Error>;

    fn try_convert_model_passthrough(
        &self,
        value: Source,
    ) -> std::result::Result<Target, Self::Error> {
        self.try_convert(value)
    }

    fn passthrough_model_for_mapper_context(&self, _target: &Target) -> Option<String> {
        None
    }

    fn postprocess_converted_req_body(
        &self,
        bytes: Bytes,
    ) -> std::result::Result<Bytes, Self::Error> {
        Ok(bytes)
    }
}

pub trait TryConvertError<Source, Target>: Sized {
    type Error;

    fn try_convert_error(
        &self,
        resp_parts: &Parts,
        value: Source,
    ) -> std::result::Result<Target, Self::Error>;
}

pub trait TryConvertStreamData<Source, Target>: Sized {
    type Error;

    /// Returns `None` if the chunk in `value` cannot be converted to an
    /// equivalent chunk in `Target`.
    fn try_convert_chunk(
        &self,
        value: Source,
        anthropic_openai_usage: Option<&AnthropicOpenAiUsageCell>,
    ) -> std::result::Result<Option<Target>, Self::Error>;
}

pub trait ResponseBodyConverter<Source, Target>: TryConvert<Source, Target> {
    fn try_convert_response(
        &self,
        _resp_parts: &Parts,
        value: Source,
    ) -> std::result::Result<Target, Self::Error> {
        self.try_convert(value)
    }
}

pub trait EndpointConverter {
    /// Convert a request body to a target request body with raw bytes.
    ///
    /// `MapperContext` is used to determine if the request is a stream
    /// since within the converter we have deserialized the request
    /// bytes to a concrete type.
    fn convert_req_body(&self, req_body_bytes: Bytes) -> Result<(Bytes, MapperContext), ApiError>;

    /// Convert a request body while preserving the client-provided model when
    /// the concrete converter supports Unified API model passthrough.
    fn convert_req_body_model_passthrough(
        &self,
        req_body_bytes: Bytes,
    ) -> Result<(Bytes, MapperContext), ApiError>;

    /// Convert a response body to a target response body with raw bytes.
    ///
    /// Returns `None` if there is no applicable mapping for a given chunk
    /// when converting stream response bodies.
    fn convert_resp_body(
        &self,
        resp_parts: Parts,
        resp_body_bytes: Bytes,
        is_stream: bool,
        anthropic_openai_usage: Option<&AnthropicOpenAiUsageCell>,
        lenient_openai_chat_completion_roles: bool,
    ) -> Result<Option<Bytes>, ApiError>;
}

pub struct TypedEndpointConverter<S, T, C>
where
    S: Endpoint,
    T: Endpoint,
    C: TryConvert<S::RequestBody, T::RequestBody>
        + ResponseBodyConverter<T::ResponseBody, S::ResponseBody>,
{
    converter: C,
    _phantom: std::marker::PhantomData<(S, T)>,
}

impl<S, T, C> TypedEndpointConverter<S, T, C>
where
    S: Endpoint,
    T: Endpoint,
    C: TryConvert<S::RequestBody, T::RequestBody>,
    C: ResponseBodyConverter<T::ResponseBody, S::ResponseBody>,
{
    pub fn new(converter: C) -> Self {
        Self {
            converter,
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<S, T, C> TypedEndpointConverter<S, T, C>
where
    S: Endpoint,
    S::RequestBody: DeserializeOwned + AiRequest,
    S::ResponseBody: Serialize,
    S::StreamResponseBody: Serialize,
    S::ErrorResponseBody: Serialize,
    T: Endpoint,
    T::RequestBody: Serialize + AiRequest,
    T::ResponseBody: DeserializeOwned,
    T::StreamResponseBody: DeserializeOwned,
    T::ErrorResponseBody: DeserializeOwned + 'static,
    C: TryConvert<S::RequestBody, T::RequestBody>,
    C: ResponseBodyConverter<T::ResponseBody, S::ResponseBody>,
    C: TryConvertStreamData<T::StreamResponseBody, S::StreamResponseBody>,
    C: TryConvertError<T::ErrorResponseBody, S::ErrorResponseBody>,
    <C as TryConvert<S::RequestBody, T::RequestBody>>::Error: Into<MapperError>,
    <C as TryConvert<T::ResponseBody, S::ResponseBody>>::Error: Into<MapperError>,
    <C as TryConvertStreamData<T::StreamResponseBody, S::StreamResponseBody>>::Error:
        Into<MapperError>,
    <C as TryConvertError<T::ErrorResponseBody, S::ErrorResponseBody>>::Error: Into<MapperError>,
{
    fn convert_req_body_with<F>(
        &self,
        bytes: Bytes,
        convert: F,
        use_passthrough_model_context: bool,
    ) -> Result<(Bytes, MapperContext), ApiError>
    where
        F: FnOnce(
            &C,
            S::RequestBody,
        ) -> std::result::Result<
            T::RequestBody,
            <C as TryConvert<S::RequestBody, T::RequestBody>>::Error,
        >,
    {
        let source_request: S::RequestBody =
            serde_json::from_slice(&bytes).map_err(InvalidRequestError::InvalidRequestBody)?;
        let is_stream = source_request.is_stream();
        let target_request = convert(&self.converter, source_request)
            .map_err(|e| InternalError::MapperError(e.into()))?;
        let model = if use_passthrough_model_context {
            self.converter
                .passthrough_model_for_mapper_context(&target_request)
                .map(ModelId::Unknown)
        } else {
            None
        }
        .map(Ok)
        .unwrap_or_else(|| {
            target_request
                .model()
                .map_err(InternalError::MapperError)
                .inspect_err(|e| {
                    tracing::error!(?e, "failed to get model from request");
                })
        })?;

        let anthropic_openai_usage = is_stream.then(|| {
            std::sync::Arc::new(std::sync::Mutex::new(
                crate::types::extensions::AnthropicStreamOpenAiUsageState::default(),
            ))
        });
        let mapper_ctx = MapperContext {
            is_stream,
            client_response_semantic: client_response_semantic_for_source_endpoint::<S>(),
            logger_response_wire_semantic: logger_response_wire_semantic_for_target_endpoint::<T>(
                is_stream,
            ),
            model: Some(model),
            anthropic_openai_usage,
            unified_responses_bridge_chat_completions_sse: false,
            native_semantic_passthrough: false,
            cursor_responses_via_chat_completions: false,
            cursor_responses_origin: None,
            client_expects_responses_wire: false,
        };
        let target_bytes = Bytes::from(serde_json::to_vec(&target_request).map_err(|e| {
            InternalError::Serialize {
                ty: std::any::type_name::<T::RequestBody>(),
                error: e,
            }
        })?);
        let target_bytes =
            <C as TryConvert<S::RequestBody, T::RequestBody>>::postprocess_converted_req_body(
                &self.converter,
                target_bytes,
            )
            .map_err(
                |e: <C as TryConvert<S::RequestBody, T::RequestBody>>::Error| {
                    InternalError::MapperError(e.into())
                },
            )?;

        Ok((target_bytes, mapper_ctx))
    }
}

impl<S, T, C> EndpointConverter for TypedEndpointConverter<S, T, C>
where
    S: Endpoint,
    S::RequestBody: DeserializeOwned + AiRequest,
    S::ResponseBody: Serialize,
    S::StreamResponseBody: Serialize,
    S::ErrorResponseBody: Serialize,
    T: Endpoint,
    T::RequestBody: Serialize + AiRequest,
    T::ResponseBody: DeserializeOwned,
    T::StreamResponseBody: DeserializeOwned,
    T::ErrorResponseBody: DeserializeOwned + 'static,
    C: TryConvert<S::RequestBody, T::RequestBody>,
    C: ResponseBodyConverter<T::ResponseBody, S::ResponseBody>,
    C: TryConvertStreamData<T::StreamResponseBody, S::StreamResponseBody>,
    C: TryConvertError<T::ErrorResponseBody, S::ErrorResponseBody>,
    <C as TryConvert<S::RequestBody, T::RequestBody>>::Error: Into<MapperError>,
    <C as TryConvert<T::ResponseBody, S::ResponseBody>>::Error: Into<MapperError>,
    <C as TryConvertStreamData<T::StreamResponseBody, S::StreamResponseBody>>::Error:
        Into<MapperError>,
    <C as TryConvertError<T::ErrorResponseBody, S::ErrorResponseBody>>::Error: Into<MapperError>,
{
    fn convert_req_body(&self, bytes: Bytes) -> Result<(Bytes, MapperContext), ApiError> {
        self.convert_req_body_with(bytes, C::try_convert, false)
    }

    fn convert_req_body_model_passthrough(
        &self,
        bytes: Bytes,
    ) -> Result<(Bytes, MapperContext), ApiError> {
        self.convert_req_body_with(bytes, C::try_convert_model_passthrough, true)
    }

    fn convert_resp_body(
        &self,
        resp_parts: Parts,
        bytes: Bytes,
        is_stream: bool,
        anthropic_openai_usage: Option<&AnthropicOpenAiUsageCell>,
        lenient_openai_chat_completion_roles: bool,
    ) -> Result<Option<Bytes>, ApiError> {
        if is_stream {
            let source_response: T::StreamResponseBody = if lenient_openai_chat_completion_roles {
                let mut value: serde_json::Value =
                    serde_json::from_slice(&bytes).map_err(|e| InternalError::Deserialize {
                        ty: std::any::type_name::<T::StreamResponseBody>(),
                        error: e,
                    })?;
                chat_completion_role_normalize::normalize_chat_completion_roles_in_place(
                    &mut value, true,
                );
                chat_completion_role_normalize::ensure_openai_chat_completion_required_fields_in_place(
                    &mut value,
                    true,
                );
                let patched = serde_json::to_vec(&value).map_err(|e| InternalError::Serialize {
                    ty: std::any::type_name::<T::StreamResponseBody>(),
                    error: e,
                })?;
                serde_json::from_slice(&patched).map_err(|e| InternalError::Deserialize {
                    ty: std::any::type_name::<T::StreamResponseBody>(),
                    error: e,
                })?
            } else {
                serde_json::from_slice(&bytes).map_err(|e| InternalError::Deserialize {
                    ty: std::any::type_name::<T::StreamResponseBody>(),
                    error: e,
                })?
            };
            let target_response: Option<S::StreamResponseBody> = self
                .converter
                .try_convert_chunk(source_response, anthropic_openai_usage)
                .map_err(|e| InternalError::MapperError(e.into()))?;

            if let Some(target_response) = target_response {
                let target_bytes =
                    serde_json::to_vec(&target_response).map_err(|e| InternalError::Serialize {
                        ty: std::any::type_name::<T::ResponseBody>(),
                        error: e,
                    })?;

                Ok(Some(Bytes::from(target_bytes)))
            } else {
                Ok(None)
            }
        } else if resp_parts.status.is_client_error() || resp_parts.status.is_server_error() {
            let source_error: T::ErrorResponseBody =
                if std::any::TypeId::of::<T::ErrorResponseBody>()
                    == std::any::TypeId::of::<WrappedError>()
                {
                    let wrapped = wrapped_error_lenient::deserialize_wrapped_error_lenient(&bytes);
                    serde_json::from_value(serde_json::to_value(&wrapped).map_err(|e| {
                        InternalError::Serialize {
                            ty: std::any::type_name::<WrappedError>(),
                            error: e,
                        }
                    })?)
                    .map_err(|e| InternalError::Deserialize {
                        ty: std::any::type_name::<T::ErrorResponseBody>(),
                        error: e,
                    })?
                } else if std::any::TypeId::of::<T::ErrorResponseBody>()
                    == std::any::TypeId::of::<
                        crate::endpoints::anthropic::messages::AnthropicApiError,
                    >()
                {
                    let anthropic =
                        anthropic_error_lenient::deserialize_anthropic_error_lenient(&bytes);
                    serde_json::from_value(serde_json::to_value(&anthropic).map_err(|e| {
                        InternalError::Serialize {
                            ty: std::any::type_name::<
                                crate::endpoints::anthropic::messages::AnthropicApiError,
                            >(),
                            error: e,
                        }
                    })?)
                    .map_err(|e| InternalError::Deserialize {
                        ty: std::any::type_name::<T::ErrorResponseBody>(),
                        error: e,
                    })?
                } else {
                    serde_json::from_slice(&bytes).map_err(|e| InternalError::Deserialize {
                        ty: std::any::type_name::<T::ErrorResponseBody>(),
                        error: e,
                    })?
                };
            let target_response: S::ErrorResponseBody = self
                .converter
                .try_convert_error(&resp_parts, source_error)
                .map_err(|e| InternalError::MapperError(e.into()))?;

            let target_bytes =
                serde_json::to_vec(&target_response).map_err(|e| InternalError::Serialize {
                    ty: std::any::type_name::<T::ResponseBody>(),
                    error: e,
                })?;

            Ok(Some(Bytes::from(target_bytes)))
        } else {
            let source_response: T::ResponseBody = if lenient_openai_chat_completion_roles {
                let mut value: serde_json::Value =
                    serde_json::from_slice(&bytes).map_err(|e| InternalError::Deserialize {
                        ty: std::any::type_name::<T::ResponseBody>(),
                        error: e,
                    })?;
                chat_completion_role_normalize::normalize_chat_completion_roles_in_place(
                    &mut value, false,
                );
                chat_completion_role_normalize::ensure_openai_chat_completion_required_fields_in_place(
                    &mut value,
                    false,
                );
                let patched = serde_json::to_vec(&value).map_err(|e| InternalError::Serialize {
                    ty: std::any::type_name::<T::ResponseBody>(),
                    error: e,
                })?;
                serde_json::from_slice(&patched).map_err(|e| InternalError::Deserialize {
                    ty: std::any::type_name::<T::ResponseBody>(),
                    error: e,
                })?
            } else {
                serde_json::from_slice(&bytes).map_err(|e| InternalError::Deserialize {
                    ty: std::any::type_name::<T::ResponseBody>(),
                    error: e,
                })?
            };
            let target_response: S::ResponseBody = self
                .converter
                .try_convert_response(&resp_parts, source_response)
                .map_err(|e| InternalError::MapperError(e.into()))?;

            let target_bytes =
                serde_json::to_vec(&target_response).map_err(|e| InternalError::Serialize {
                    ty: std::any::type_name::<T::ResponseBody>(),
                    error: e,
                })?;

            Ok(Some(Bytes::from(target_bytes)))
        }
    }
}

pub(crate) fn openai_error_from_status(
    status_code: StatusCode,
    message: Option<String>,
) -> WrappedError {
    let kind = self::openai::get_error_type(status_code);
    let code = self::openai::get_error_code(status_code);
    let message = message.unwrap_or_else(|| kind.clone());

    async_openai::error::WrappedError {
        error: async_openai::error::ApiError {
            message,
            code,
            param: None,
            r#type: Some(kind),
        },
    }
}

pub(super) fn mime_from_data_uri(uri: &str) -> Option<infer::Type> {
    // Split on the first comma.  If no comma => not a data-URI.
    let (_first, b64) = uri.split_once(',')?;

    // Only decode the first portion of base64 data for efficiency.
    // Base64 has 4:3 ratio, so 64 chars -> ~48 bytes, which is plenty for MIME
    // detection.
    let b64_prefix = &b64[..b64.len().min(64)];

    // Decode only the prefix into a fixed buffer.
    let mut header = [0u8; 48];
    let n = base64::engine::general_purpose::STANDARD
        .decode_slice(b64_prefix.as_bytes(), &mut header)
        .ok()?;

    infer::get(&header[..n])
}

#[cfg(test)]
mod request_envelope_tests {
    use async_openai::types::CreateChatCompletionRequest;
    use serde_json::json;

    use crate::{
        endpoints::{ApiEndpoint, openai::OpenAI},
        middleware::mapper::profile_resolver::resolve_mapper_metadata,
        types::provider::InferenceProvider,
    };

    #[test]
    fn request_envelope_captures_openai_chat_request_metadata() {
        let request: CreateChatCompletionRequest = serde_json::from_value(json!({
            "model": "openai/gpt-4o-mini",
            "messages": [
                {
                    "role": "user",
                    "content": "hello"
                }
            ],
            "stream": true
        }))
        .expect("request should deserialize");

        let envelope =
            crate::middleware::mapper::envelope::RequestEnvelope::from_openai_chat_request(
                ApiEndpoint::OpenAI(OpenAI::chat_completions()),
                InferenceProvider::Anthropic,
                request,
            );

        assert_eq!(envelope.raw_model, "openai/gpt-4o-mini");
        assert!(envelope.is_stream);
        assert_eq!(envelope.target_provider, InferenceProvider::Anthropic);
    }

    #[test]
    fn request_rule_engine_applies_named_provider_rules_to_non_stream_openai_envelope() {
        let request: CreateChatCompletionRequest = serde_json::from_value(json!({
            "model": "qwen/qwen3-32b",
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
            "tool_choice": "auto",
            "parallel_tool_calls": true,
            "response_format": {
                "type": "json_object"
            },
            "reasoning_effort": "high"
        }))
        .expect("request should deserialize");

        let provider = InferenceProvider::Named("qwen".into());
        let envelope =
            crate::middleware::mapper::envelope::RequestEnvelope::from_openai_chat_request(
                ApiEndpoint::OpenAI(OpenAI::chat_completions()),
                provider.clone(),
                request,
            )
            .with_resolved_metadata(
                resolve_mapper_metadata(&provider, Some("qwen/qwen3-32b"))
                    .expect("metadata should resolve"),
            );

        let prepared =
            crate::middleware::mapper::request_rule_engine::prepare_request_envelope(envelope)
                .expect("request rule engine should succeed");

        assert!(prepared.request_rule_context.is_some());
        assert!(prepared.openai_request.tools.is_some());
        assert_eq!(
            prepared.openai_request.tool_choice,
            Some(async_openai::types::ChatCompletionToolChoiceOption::Auto)
        );
        assert_eq!(prepared.openai_request.parallel_tool_calls, Some(true));
        assert_eq!(
            prepared
                .openai_request
                .response_format
                .as_ref()
                .and_then(|value| serde_json::to_value(value).ok())
                .and_then(|value| value.get("type").cloned()),
            Some(json!("json_object"))
        );
        assert!(prepared.openai_request.reasoning_effort.is_none());
    }
}
