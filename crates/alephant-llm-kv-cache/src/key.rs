use sha2::{Digest, Sha256};

fn body_for_cache(body: &str, ignore_keys: &[String]) -> String {
    let Ok(mut v) = serde_json::from_str::<serde_json::Value>(body) else {
        return body.to_string();
    };
    if let serde_json::Value::Object(ref mut m) = v {
        for k in ignore_keys {
            m.remove(k);
        }
    }
    v.to_string()
}

/// SHA-256 hex cache key derived from request content only.
///
/// Hash material: `cache_seed ‖ url ‖ body ‖ bucket_index?`
#[must_use]
pub fn kv_key_sha256_hex(
    cache_seed: &str,
    url: &str,
    body: &str,
    cache_ignore_keys: &[String],
    bucket_index: u8,
) -> String {
    let body = body_for_cache(body, cache_ignore_keys);
    let mut material = String::new();
    material.push_str(cache_seed);
    material.push_str(url);
    material.push_str(&body);
    if bucket_index >= 1 {
        material.push_str(&bucket_index.to_string());
    }
    let mut hasher = Sha256::new();
    hasher.update(material.as_bytes());
    let out = hasher.finalize();
    format!("{out:x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_content_produces_same_key() {
        let k1 = kv_key_sha256_hex(
            "seed",
            "https://example.com/v1",
            r#"{"model":"gpt-4","messages":[{"role":"user","content":"hi"}]}"#,
            &[],
            0,
        );
        let k2 = kv_key_sha256_hex(
            "seed",
            "https://example.com/v1",
            r#"{"model":"gpt-4","messages":[{"role":"user","content":"hi"}]}"#,
            &[],
            0,
        );
        assert_eq!(k1, k2);
    }

    #[test]
    fn different_bucket_index_produces_different_key() {
        let k0 = kv_key_sha256_hex(
            "",
            "https://example.com/v1",
            r#"{"x":1}"#,
            &[],
            0,
        );
        let k1 = kv_key_sha256_hex(
            "",
            "https://example.com/v1",
            r#"{"x":1}"#,
            &[],
            1,
        );
        let k2 = kv_key_sha256_hex(
            "",
            "https://example.com/v1",
            r#"{"x":1}"#,
            &[],
            2,
        );
        assert_ne!(k0, k1);
        assert_ne!(k1, k2);
        assert_ne!(k0, k2);
    }

    #[test]
    fn different_seed_produces_different_key() {
        let k1 = kv_key_sha256_hex(
            "seed-a",
            "https://example.com/v1",
            r#"{"x":1}"#,
            &[],
            0,
        );
        let k2 = kv_key_sha256_hex(
            "seed-b",
            "https://example.com/v1",
            r#"{"x":1}"#,
            &[],
            0,
        );
        assert_ne!(k1, k2);
    }

    #[test]
    fn cache_ignore_keys_strips_body_field() {
        let k1 = kv_key_sha256_hex(
            "",
            "https://example.com/v1",
            r#"{"model":"gpt-4","stream":true}"#,
            &["stream".to_string()],
            0,
        );
        let k2 = kv_key_sha256_hex(
            "",
            "https://example.com/v1",
            r#"{"model":"gpt-4"}"#,
            &[],
            0,
        );
        assert_eq!(k1, k2);
    }
}
