use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltKey {
    pub embed_text: String,
    pub params_hash: String,
    pub cache_key: String,
}

pub fn build_cache_key(
    path: &str,
    body: &[u8],
    embedder_identity: &str,
) -> Result<BuiltKey, String> {
    let root: Value = serde_json::from_slice(body)
        .map_err(|_| "invalid json body".to_string())?;
    let embed_text = extract_last_user_text(&root)
        .ok_or_else(|| "no user text".to_string())?;
    let params_hash = compute_params_hash(path, &root, embedder_identity);
    let cache_key = sha256_hex(&format!("{embed_text}\0{params_hash}"));
    Ok(BuiltKey {
        embed_text,
        params_hash,
        cache_key,
    })
}

pub fn extract_embed_text_from_body(body: &[u8]) -> Result<String, String> {
    let root: Value = serde_json::from_slice(body)
        .map_err(|_| "invalid json body".to_string())?;
    extract_last_user_text(&root).ok_or_else(|| "no user text".to_string())
}

fn compute_params_hash(
    path: &str,
    root: &Value,
    embedder_identity: &str,
) -> String {
    let model = root
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let temperature = number_field(root, "temperature");
    let top_p = number_field(root, "top_p");
    let max_tokens = int_field(root, "max_tokens");
    let max_completion_tokens = int_field(root, "max_completion_tokens");
    let stream = root.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let payload = format!(
        "path={path}\0model={model}\0temperature={temperature}\0top_p={top_p}\\
         \
         0max_tokens={max_tokens}\\
         0max_completion_tokens={max_completion_tokens}\0stream={stream}\\
         0embedder={embedder_identity}"
    );
    sha256_hex(&payload)
}

fn number_field(root: &Value, key: &str) -> String {
    root.get(key)
        .and_then(Value::as_f64)
        .map(|v| v.to_string())
        .unwrap_or_default()
}

fn int_field(root: &Value, key: &str) -> String {
    root.get(key)
        .and_then(Value::as_i64)
        .map(|v| v.to_string())
        .unwrap_or_default()
}

fn extract_last_user_text(root: &Value) -> Option<String> {
    let messages = root.get("messages")?.as_array()?;
    for m in messages.iter().rev() {
        if m.get("role").and_then(Value::as_str) != Some("user") {
            continue;
        }
        let content = m.get("content")?;
        if let Some(s) = content.as_str() {
            let t = s.trim();
            if !t.is_empty() {
                return Some(t.to_string());
            }
        }
        if let Some(parts) = content.as_array() {
            let mut merged = String::new();
            for p in parts {
                if p.get("type").and_then(Value::as_str) != Some("text") {
                    continue;
                }
                if let Some(txt) = p.get("text").and_then(Value::as_str) {
                    let trimmed = txt.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    if !merged.is_empty() {
                        merged.push(' ');
                    }
                    merged.push_str(trimmed);
                }
            }
            if !merged.is_empty() {
                return Some(merged);
            }
        }
    }
    None
}

fn sha256_hex(input: &str) -> String {
    let mut h = Sha256::new();
    h.update(input.as_bytes());
    let out = h.finalize();
    let mut s = String::with_capacity(out.len() * 2);
    for b in out {
        use std::fmt::Write as _;
        let _ = write!(&mut s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::{build_cache_key, extract_embed_text_from_body};

    #[test]
    fn build_cache_key_changes_when_temperature_changes() {
        let body_a = br#"{"model":"gpt-4o-mini","temperature":0.1,"messages":[{"role":"user","content":"hello"}]}"#;
        let body_b = br#"{"model":"gpt-4o-mini","temperature":0.9,"messages":[{"role":"user","content":"hello"}]}"#;
        let k1 = build_cache_key(
            "/v1/chat/completions",
            body_a,
            "openai|https://api.openai.com",
        )
        .unwrap();
        let k2 = build_cache_key(
            "/v1/chat/completions",
            body_b,
            "openai|https://api.openai.com",
        )
        .unwrap();
        assert_ne!(k1.cache_key, k2.cache_key);
        assert_ne!(k1.params_hash, k2.params_hash);
    }

    #[test]
    fn extract_embed_text_uses_last_user_message() {
        let body = br#"{"messages":[{"role":"user","content":"first"},{"role":"assistant","content":"ok"},{"role":"user","content":"second"}]}"#;
        let k = build_cache_key(
            "/v1/chat/completions",
            body,
            "openai|https://api.openai.com",
        )
        .unwrap();
        assert_eq!(k.embed_text, "second");
    }

    #[test]
    fn extract_embed_text_from_body_returns_last_user_text() {
        let text = extract_embed_text_from_body(
            br#"{"messages":[{"role":"user","content":"first"},{"role":"user","content":"second"}]}"#,
        )
        .unwrap();

        assert_eq!(text, "second");
    }

    #[test]
    fn params_hash_changes_when_embedding_dimension_changes() {
        let body =
            br#"{"model":"gpt-4o-mini","messages":[{"role":"user","content":"hello"}]}"#;
        let k1 = build_cache_key(
            "/v1/chat/completions",
            body,
            "provider=openai\0model=text-embedding-3\0base_url=https://api.openai.com\0dimension=1536",
        )
        .unwrap();
        let k2 = build_cache_key(
            "/v1/chat/completions",
            body,
            "provider=openai\0model=text-embedding-3\0base_url=https://api.openai.com\0dimension=3072",
        )
        .unwrap();

        assert_ne!(k1.params_hash, k2.params_hash);
    }
}
