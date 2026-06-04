use std::collections::HashMap;

use bytes::Bytes;
use http::StatusCode;
use tokio::sync::oneshot;

use crate::{
    error::{api::ApiError, internal::InternalError},
    types::{
        body::{BodyReader, TfftTrigger},
        extensions::MapperContext,
    },
};

pub(crate) fn llm_kv_slot_keys(
    settings: &alephant_llm_kv_cache::CacheSettings,
    target_url: &url::Url,
    body: &Bytes,
) -> Vec<String> {
    let body_str = String::from_utf8_lossy(body).into_owned();
    let mut keys = Vec::new();
    for i in 0..settings.bucket_size {
        let k = alephant_llm_kv_cache::kv_key_sha256_hex(
            settings.cache_seed.as_deref().unwrap_or(""),
            target_url.as_str(),
            &body_str,
            &[],
            i,
        );
        keys.push(k);
    }
    keys
}

pub(crate) fn llm_kv_write_slot_keys(
    settings: &alephant_llm_kv_cache::CacheSettings,
    effective_target_url: &url::Url,
    effective_request_body: &Bytes,
) -> Vec<String> {
    llm_kv_slot_keys(settings, effective_target_url, effective_request_body)
}

pub(crate) fn semantic_write_body_bytes(
    original_request_body: &Bytes,
    _effective_request_body: &Bytes,
) -> Vec<u8> {
    original_request_body.to_vec()
}

pub(crate) fn build_llm_cache_hit_response(
    entry: &alephant_llm_kv_cache::LlmCacheEntry,
    bucket_idx: usize,
    mapper_ctx: &MapperContext,
) -> Result<
    (
        http::Response<crate::types::body::Body>,
        BodyReader,
        oneshot::Receiver<()>,
    ),
    ApiError,
> {
    let entry = entry.clone();
    let mut resp_builder = http::Response::builder().status(StatusCode::OK);
    let hm = resp_builder.headers_mut().unwrap();
    let hdrs: HashMap<String, String> = entry.headers.clone();
    alephant_llm_kv_cache::merge_cached_headers(hm, &hdrs);
    alephant_llm_kv_cache::apply_alephant_cache_hit_headers(hm, bucket_idx, entry.latency);
    let chunks: Vec<Bytes> = entry
        .body
        .iter()
        .map(|s| Bytes::copy_from_slice(s.as_bytes()))
        .collect();
    let stream = futures::stream::iter(chunks.into_iter().map(Ok::<_, ApiError>));
    let append_nl = mapper_ctx.is_stream;
    let tfft = if mapper_ctx.is_stream {
        TfftTrigger::FirstModelToken
    } else {
        TfftTrigger::Never
    };
    let (body, reader, tfft_rx) = BodyReader::wrap_stream(stream, append_nl, tfft, None);
    let response = resp_builder.body(body).map_err(InternalError::HttpError)?;
    Ok((response, reader, tfft_rx))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use bytes::Bytes;

    use super::*;
    use crate::types::extensions::{
        ClientResponseSemantic, LoggerResponseWireSemantic, MapperContext,
    };

    fn test_settings() -> alephant_llm_kv_cache::CacheSettings {
        alephant_llm_kv_cache::CacheSettings {
            should_read: false,
            should_write: true,
            cache_control_value: "public, max-age=60".to_string(),
            bucket_size: 2,
            cache_seed: Some("seed".to_string()),
        }
    }

    fn mapper_ctx(is_stream: bool) -> MapperContext {
        MapperContext {
            is_stream,
            client_response_semantic: ClientResponseSemantic::Other,
            logger_response_wire_semantic: LoggerResponseWireSemantic::Other,
            model: None,
            anthropic_openai_usage: None,
            unified_responses_bridge_chat_completions_sse: false,
            native_semantic_passthrough: false,
            cursor_responses_via_chat_completions: false,
            cursor_responses_origin: None,
            client_expects_responses_wire: false,
        }
    }

    #[test]
    fn cache_write_keys_follow_effective_request_after_fallback() {
        let settings = test_settings();
        let original_url =
            url::Url::parse("https://openai.test/v1/chat/completions").expect("original url");
        let effective_url =
            url::Url::parse("https://groq.test/v1/chat/completions").expect("effective url");
        let original_body = Bytes::from(
            serde_json::json!({
                "model": "openai/gpt-5.4",
                "messages": [{"role": "user", "content": "hi"}]
            })
            .to_string(),
        );
        let effective_body = Bytes::from(
            serde_json::json!({
                "model": "groq/llama-3.1-8b",
                "messages": [{"role": "user", "content": "hi"}]
            })
            .to_string(),
        );

        let original_keys = llm_kv_slot_keys(&settings, &original_url, &original_body);
        let effective_keys = llm_kv_write_slot_keys(&settings, &effective_url, &effective_body);

        assert_ne!(effective_keys, original_keys);
        assert_eq!(
            effective_keys,
            llm_kv_slot_keys(&settings, &effective_url, &effective_body)
        );
    }

    #[test]
    fn semantic_write_body_uses_original_request_body() {
        let original_body = Bytes::from(
            serde_json::json!({
                "model": "openai/gpt-4",
                "messages": [{"role": "user", "content": "original"}]
            })
            .to_string(),
        );
        let effective_body = Bytes::from(
            serde_json::json!({
                "model": "google/gemini-2.5-pro",
                "messages": [{"role": "user", "content": "effective"}]
            })
            .to_string(),
        );

        let semantic_body = semantic_write_body_bytes(&original_body, &effective_body);
        assert_eq!(semantic_body, original_body.to_vec());
    }

    #[test]
    fn build_llm_cache_hit_response_applies_cache_hit_headers() {
        let entry = alephant_llm_kv_cache::LlmCacheEntry {
            headers: HashMap::from([("content-type".to_string(), "application/json".to_string())]),
            latency: 42,
            body: vec![r#"{"ok":true}"#.to_string()],
        };

        let (response, _reader, _tfft_rx) =
            build_llm_cache_hit_response(&entry, 1, &mapper_ctx(false))
                .expect("cache hit response");

        assert_eq!(response.status(), http::StatusCode::OK);
        assert_eq!(
            response.headers().get(http::header::CONTENT_TYPE),
            Some(&http::HeaderValue::from_static("application/json"))
        );
        assert_eq!(
            response.headers().get("alephant-cache"),
            Some(&http::HeaderValue::from_static("HIT"))
        );
        assert_eq!(
            response.headers().get("alephant-cache-bucket-idx"),
            Some(&http::HeaderValue::from_static("1"))
        );
        assert_eq!(
            response.headers().get("alephant-cache-latency"),
            Some(&http::HeaderValue::from_static("42"))
        );
    }
}
