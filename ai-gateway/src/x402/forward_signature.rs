use bytes::Bytes;
use hmac::{Hmac, Mac};
use http::{HeaderMap, HeaderValue, Method};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::app_state::AppState;

type HmacSha256 = Hmac<Sha256>;

pub const ALEPHANT_TIMESTAMP_HEADER: &str = "x-alephant-timestamp";
pub const ALEPHANT_SIGNATURE_HEADER: &str = "x-alephant-signature";
pub const ALEPHANT_ENDPOINT_ID_HEADER: &str = "x-alephant-endpoint-id";
const ENDPOINT_SECRET_REDIS_KEY_PREFIX: &str = "lc:x402:secret:endpoint:";

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ForwardSignatureHeaderError {
    #[error("invalid x-alephant-timestamp header value")]
    InvalidTimestamp,
    #[error("invalid x-alephant-signature header value")]
    InvalidSignature,
    #[error("invalid x-alephant-endpoint-id header value")]
    InvalidEndpointId,
}

#[derive(Debug, Error)]
pub enum ForwardSignatureSecretError {
    #[error("x402 endpoint signing secret not found")]
    NotFound,
    #[error("x402 endpoint signing secret store unavailable")]
    StoreUnavailable,
    #[error("x402 endpoint signing secret query failed: {0}")]
    Query(sqlx::Error),
    #[error("x402 endpoint signing secret decrypt failed: {0}")]
    Decrypt(#[from] crate::x402::secret::X402SecretDecryptError),
}

#[must_use]
pub fn endpoint_secret_redis_key(endpoint_id: Uuid) -> String {
    format!("{ENDPOINT_SECRET_REDIS_KEY_PREFIX}{endpoint_id}")
}

#[must_use]
pub fn upstream_path_with_query(url: &reqwest::Url) -> String {
    match url.query() {
        Some(query) if !query.is_empty() => format!("{}?{}", url.path(), query),
        _ => url.path().to_string(),
    }
}

pub async fn resolve_endpoint_signing_secret(
    app_state: &AppState,
    endpoint_id: Uuid,
) -> Result<Vec<u8>, ForwardSignatureSecretError> {
    let redis_key = endpoint_secret_redis_key(endpoint_id);
    if let Some(redis) = app_state.redis() {
        match redis.get_string(&redis_key).await {
            Ok(Some(secret)) if !secret.trim().is_empty() => {
                let secret = secret.into_bytes();
                log_resolved_secret_metadata(endpoint_id, "redis", &secret);
                return Ok(secret);
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(
                    endpoint_id = %endpoint_id,
                    redis_key = %redis_key,
                    error = %error,
                    "x402 endpoint signing secret redis lookup failed; falling back to DB"
                );
            }
        }
    }

    let store = app_state
        .router_store()
        .ok_or(ForwardSignatureSecretError::StoreUnavailable)?;
    let Some(secret_ciphertext) = store
        .fetch_active_x402_endpoint_secret_ciphertext(endpoint_id)
        .await
        .map_err(ForwardSignatureSecretError::Query)?
    else {
        return Err(ForwardSignatureSecretError::NotFound);
    };
    let secret =
        crate::x402::secret::decrypt_secret_from_env(&secret_ciphertext)?;
    log_resolved_secret_metadata(endpoint_id, "db", &secret);
    Ok(secret)
}

fn log_resolved_secret_metadata(
    endpoint_id: Uuid,
    source: &'static str,
    secret: &[u8],
) {
    tracing::info!(
        endpoint_id = %endpoint_id,
        source,
        secret_len = secret.len(),
        "x402 endpoint signing secret resolved"
    );
}

#[must_use]
pub fn canonical_forward_signature_string(
    timestamp: &str,
    method: &Method,
    path_with_query: &str,
    body: &Bytes,
) -> String {
    format!(
        "v2\n{}\n{}\n{}\n{}",
        timestamp,
        method.as_str().to_ascii_uppercase(),
        path_with_query,
        hex::encode(Sha256::digest(body))
    )
}

#[must_use]
pub fn sign_forwarded_request(
    endpoint_secret: &[u8],
    timestamp: &str,
    method: &Method,
    path_with_query: &str,
    body: &Bytes,
) -> String {
    let canonical = canonical_forward_signature_string(
        timestamp,
        method,
        path_with_query,
        body,
    );
    let mut mac = HmacSha256::new_from_slice(endpoint_secret)
        .expect("HMAC accepts keys of any size");
    mac.update(canonical.as_bytes());
    format!("v2={}", hex::encode(mac.finalize().into_bytes()))
}

pub fn inject_forward_signature_headers(
    headers: &mut HeaderMap,
    endpoint_id: Uuid,
    endpoint_secret: &[u8],
    timestamp: &str,
    method: &Method,
    path_with_query: &str,
    body: &Bytes,
) -> Result<(), ForwardSignatureHeaderError> {
    let signature = sign_forwarded_request(
        endpoint_secret,
        timestamp,
        method,
        path_with_query,
        body,
    );
    let timestamp = HeaderValue::from_str(timestamp)
        .map_err(|_| ForwardSignatureHeaderError::InvalidTimestamp)?;
    let signature = HeaderValue::from_str(&signature)
        .map_err(|_| ForwardSignatureHeaderError::InvalidSignature)?;
    let endpoint_id = HeaderValue::from_str(&endpoint_id.to_string())
        .map_err(|_| ForwardSignatureHeaderError::InvalidEndpointId)?;

    headers.insert(ALEPHANT_TIMESTAMP_HEADER, timestamp);
    headers.insert(ALEPHANT_SIGNATURE_HEADER, signature);
    headers.insert(ALEPHANT_ENDPOINT_ID_HEADER, endpoint_id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use http::{HeaderMap, HeaderValue, Method};
    use uuid::Uuid;

    use super::*;

    #[test]
    fn canonical_string_uses_v2_timestamp_method_path_query_and_body_hash() {
        let canonical = canonical_forward_signature_string(
            "1760000000",
            &Method::GET,
            "/xx?bdd=123&ftty=345&ayjj=234",
            &Bytes::new(),
        );

        assert_eq!(
            canonical,
            concat!(
                "v2\n",
                "1760000000\n",
                "GET\n",
                "/xx?bdd=123&ftty=345&ayjj=234\n",
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
            )
        );
    }

    #[test]
    fn forwarded_signature_is_v2_hmac_sha256_hex() {
        let signature = sign_forwarded_request(
            b"test-secret",
            "1760000000",
            &Method::POST,
            "/xx?bdd=123",
            &Bytes::from_static(br#"{"hello":"world"}"#),
        );

        assert_eq!(signature.len(), 67);
        assert!(signature.starts_with("v2="));
        assert_eq!(
            signature,
            "v2=cd1663cef515ca62e4b89dddd7e525ff86807e8435e6e7582fad3c9cfadc6faa"
        );
    }

    #[test]
    fn inject_forward_signature_headers_sets_expected_alephant_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("x-keep", HeaderValue::from_static("yes"));
        let endpoint_id =
            Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();

        inject_forward_signature_headers(
            &mut headers,
            endpoint_id,
            b"test-secret",
            "1760000000",
            &Method::GET,
            "/xx?bdd=123&ftty=345&ayjj=234",
            &Bytes::new(),
        )
        .unwrap();

        assert_eq!(headers.get("x-keep").unwrap(), "yes");
        assert_eq!(headers.get("x-alephant-timestamp").unwrap(), "1760000000");
        assert_eq!(
            headers.get("x-alephant-endpoint-id").unwrap(),
            "11111111-1111-1111-1111-111111111111"
        );
        assert_eq!(
            headers.get("x-alephant-signature").unwrap(),
            "v2=2def7a165a559ca50d5d1179cdbfaf2f6497b6b63173049efc0ee271834a74f3"
        );
    }

    #[test]
    fn endpoint_secret_redis_key_uses_endpoint_id() {
        let endpoint_id =
            Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();

        assert_eq!(
            endpoint_secret_redis_key(endpoint_id),
            "lc:x402:secret:endpoint:11111111-1111-1111-1111-111111111111"
        );
    }

    #[test]
    fn upstream_path_with_query_preserves_original_query_order() {
        let url = reqwest::Url::parse(
            "https://origin.test/xx?bdd=123&ftty=345&ayjj=234",
        )
        .unwrap();

        assert_eq!(
            upstream_path_with_query(&url),
            "/xx?bdd=123&ftty=345&ayjj=234"
        );
    }
}
