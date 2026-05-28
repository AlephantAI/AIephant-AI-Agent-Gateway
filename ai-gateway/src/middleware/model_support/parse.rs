//! Parse `model` from JSON body and build catalog Redis keys.

/// Max bytes read for model catalog validation (aligned with policy body cap).
pub const MODEL_SUPPORT_MAX_BODY_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParsedProviderModel<'a> {
    pub provider_raw: &'a str,
    pub model_raw: &'a str,
}

/// Returns `None` if JSON has no non-empty `model` string — caller skips
/// validation.
#[must_use]
pub fn model_field_from_json_body(body: &[u8]) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    let s = v.get("model")?.as_str()?;
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    Some(t.to_string())
}

/// Split `provider/model` on the first `/`. Fails if `/` is missing or a side
/// is empty after trim.
pub fn split_provider_model(
    model: &str,
) -> Result<ParsedProviderModel<'_>, ()> {
    let (a, b) = model.split_once('/').ok_or(())?;
    let a = a.trim();
    let b = b.trim();
    if a.is_empty() || b.is_empty() {
        return Err(());
    }
    Ok(ParsedProviderModel {
        provider_raw: a,
        model_raw: b,
    })
}

/// Redis catalog key: `ascii_lower(code)::ascii_lower(model_id)` (design A).
#[must_use]
pub fn catalog_redis_key(provider_raw: &str, model_raw: &str) -> String {
    format!(
        "{}::{}",
        provider_raw.to_ascii_lowercase(),
        model_raw.to_ascii_lowercase()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_field_extracts() {
        let body = br#"{"model":"openai/gpt-4","messages":[]}"#;
        assert_eq!(
            model_field_from_json_body(body).as_deref(),
            Some("openai/gpt-4")
        );
    }

    #[test]
    fn case_insensitive_key() {
        let m = "OPENAI/GPT-4";
        let p = split_provider_model(m).unwrap();
        assert_eq!(
            catalog_redis_key(p.provider_raw, p.model_raw),
            "openai::gpt-4"
        );
    }

    #[test]
    fn reject_no_slash() {
        assert!(split_provider_model("gpt-4").is_err());
    }
}
