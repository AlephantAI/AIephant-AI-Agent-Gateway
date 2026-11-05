use bytes::Bytes;

/// 1 MiB (matches design §2).
pub const LARGE_BODY_THRESHOLD_BYTES: usize = 1024 * 1024;

#[must_use]
pub fn clamp_body_ttl_days(days: u16) -> u16 {
    let clamped = days.clamp(1, 730);
    if !(1..=730).contains(&days) {
        tracing::warn!(
            raw_body_ttl_days = days,
            clamped_body_ttl_days = clamped,
            "body_ttl_days out of range; clamped to 1..=730"
        );
    }
    clamped
}

/// If either side ≥ threshold → `"s3"` (Cloud: **both** bodies go to object
/// storage), else `"clickhouse"` (inline on both sides).
#[must_use]
pub fn storage_location_for_sizes(req_len: usize, resp_len: usize) -> &'static str {
    if req_len >= LARGE_BODY_THRESHOLD_BYTES || resp_len >= LARGE_BODY_THRESHOLD_BYTES {
        "s3"
    } else {
        "clickhouse"
    }
}

#[must_use]
pub fn inline_body_for_log(body: &Bytes) -> String {
    String::from_utf8_lossy(body.as_ref()).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_location_boundary_below_threshold() {
        assert_eq!(
            storage_location_for_sizes(LARGE_BODY_THRESHOLD_BYTES - 1, 0),
            "clickhouse"
        );
    }

    #[test]
    fn storage_location_boundary_at_threshold() {
        assert_eq!(
            storage_location_for_sizes(LARGE_BODY_THRESHOLD_BYTES, 0),
            "s3"
        );
    }

    #[test]
    fn clamp_body_ttl_days_zero_to_one() {
        assert_eq!(clamp_body_ttl_days(0), 1);
    }

    #[test]
    fn clamp_body_ttl_days_731_to_730() {
        assert_eq!(clamp_body_ttl_days(731), 730);
    }
}

/// Storage view for routes with payload validation; never changes usage extraction.
pub(crate) fn protected_log_view(
    _request: &Bytes,
    response: &Bytes,
    is_stream: bool,
) -> (Bytes, Bytes) {
    use serde_json::{Value, json};
    fn usage(root: &Value) -> Option<Value> {
        let object = root.get("usage")?.as_object()?;
        let mut output = serde_json::Map::new();
        for name in [
            "prompt_tokens",
            "completion_tokens",
            "input_tokens",
            "output_tokens",
            "total_tokens",
            "cache_read_input_tokens",
            "cache_write_input_tokens",
            "seconds",
        ] {
            if let Some(value) = object.get(name).filter(|v| v.is_number()) {
                output.insert(name.into(), value.clone());
            }
        }
        for name in [
            "prompt_tokens_details",
            "completion_tokens_details",
            "input_tokens_details",
            "output_tokens_details",
        ] {
            if let Some(details) = object.get(name).and_then(Value::as_object) {
                let selected: serde_json::Map<String, Value> = details
                    .iter()
                    .filter(|(k, v)| {
                        v.is_number()
                            && matches!(
                                k.as_str(),
                                "cached_tokens"
                                    | "cache_write_tokens"
                                    | "cache_write_input_tokens"
                                    | "audio_tokens"
                                    | "reasoning_tokens"
                            )
                    })
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                if !selected.is_empty() {
                    output.insert(name.into(), Value::Object(selected));
                }
            }
        }
        if let Some(value) = object
            .get("type")
            .filter(|v| matches!(v.as_str(), Some("tokens" | "duration")))
        {
            output.insert("type".into(), value.clone());
        }
        Some(Value::Object(output))
    }
    let mut safe_usage = serde_json::from_slice::<Value>(response)
        .ok()
        .as_ref()
        .and_then(usage);
    if is_stream || safe_usage.is_none() {
        if let Ok(text) = std::str::from_utf8(response) {
            for line in text.lines() {
                if let Some(data) = line.trim().strip_prefix("data:") {
                    if let Ok(value) = serde_json::from_str::<Value>(data.trim()) {
                        if let Some(value) = usage(&value) {
                            safe_usage = Some(value);
                        }
                    }
                }
            }
        }
    }
    let mut output = json!({"payload_redacted":true});
    if let Some(usage) = safe_usage {
        output["usage"] = usage;
    }
    (
        Bytes::from_static(b"{\"payload_redacted\":true}"),
        Bytes::from(serde_json::to_vec(&output).expect("safe log JSON")),
    )
}

#[cfg(test)]
mod review_privacy_tests {
    use super::*;
    #[test]
    fn final_review_validated_storage_views_remove_assets_but_keep_real_usage() {
        for is_stream in [false, true] {
            let request =
                Bytes::from_static(b"--multipart-private-asset\r\n\xff\0private-input\r\n");
            let json=br#"{"image":"private-output-asset","usage":{"input_tokens":17,"output_tokens":3,"input_tokens_details":{"cached_tokens":2,"audio_tokens":1},"seconds":1.5,"type":"duration","private":"private-usage-asset"}}"#;
            let response = if is_stream {
                Bytes::from([b"data: ".as_slice(), json, b"\n\n"].concat())
            } else {
                Bytes::copy_from_slice(json)
            };
            let before = crate::logger::usage_parse::usage_counts_from_response_body_for_log(
                is_stream, &response,
            );
            let (request, response) = protected_log_view(&request, &response, is_stream);
            assert!(!inline_body_for_log(&request).contains("private"));
            assert!(!inline_body_for_log(&response).contains("private"));
            assert_eq!(
                crate::logger::usage_parse::usage_counts_from_response_body_for_log(
                    is_stream, &response
                ),
                before
            );
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&request).unwrap()["payload_redacted"],
                true
            );
        }
    }
}
