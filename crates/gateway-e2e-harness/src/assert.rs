//! Evaluate [`crate::case::AssertionsSpec`] against
//! [`crate::captured::CapturedResponse`].

use regex::Regex;

use crate::{
    captured::CapturedResponse,
    case::{
        AssertionsSpec, BodyAssertions, HeadersAssertions, HttpStatusSpec, JsonAssertions,
        SseAssertions,
    },
};

pub fn assert_response(spec: &AssertionsSpec, cap: &CapturedResponse) -> Result<(), String> {
    if let Some(ref hs) = spec.http_status {
        match hs {
            HttpStatusSpec::Exact(code) => {
                if cap.status != *code {
                    return Err(format!("httpStatus: expected {code}, got {}", cap.status));
                }
            }
            HttpStatusSpec::In { in_list } => {
                if !in_list.contains(&cap.status) {
                    return Err(format!(
                        "httpStatus: expected one of {in_list:?}, got {}",
                        cap.status
                    ));
                }
            }
        }
    }

    if let Some(ref h) = spec.headers {
        assert_headers(h, cap)?;
    }

    if let Some(ref b) = spec.body {
        assert_body(b, cap)?;
    }

    if let Some(ref j) = spec.json {
        assert_json(j, cap)?;
    }

    if let Some(ref s) = spec.sse {
        assert_sse(s, cap)?;
    }

    Ok(())
}

fn assert_body(spec: &BodyAssertions, cap: &CapturedResponse) -> Result<(), String> {
    if let Some(ref parts) = spec.contains {
        for needle in parts {
            if !cap.body.contains(needle.as_str()) {
                return Err(format!("body.contains: missing substring {needle:?}"));
            }
        }
    }
    if let Some(ref parts) = spec.not_contains {
        for needle in parts {
            if cap.body.contains(needle.as_str()) {
                return Err(format!(
                    "body.not_contains: forbidden substring {needle:?} present"
                ));
            }
        }
    }
    if let Some(ref pattern) = spec.regex {
        let re = Regex::new(pattern)
            .map_err(|e| format!("body.regex: invalid pattern {pattern:?}: {e}"))?;
        if !re.is_match(&cap.body) {
            return Err(format!(
                "body.regex: pattern {pattern:?} did not match body"
            ));
        }
    }
    Ok(())
}

fn assert_json(spec: &JsonAssertions, cap: &CapturedResponse) -> Result<(), String> {
    let v: serde_json::Value = serde_json::from_str(&cap.body)
        .map_err(|e| format!("json: failed to parse body as JSON: {e}"))?;

    if let Some(ref paths) = spec.path_exists {
        for p in paths {
            if v.pointer(p).is_none() {
                return Err(format!(
                    "json.pathExists: pointer {p:?} missing in response"
                ));
            }
        }
    }

    if let Some(ref m) = spec.path_equals {
        for (p, expected) in m {
            let got = v
                .pointer(p)
                .ok_or_else(|| format!("json.pathEquals: pointer {p:?} missing in response"))?;
            if got != expected {
                return Err(format!(
                    "json.pathEquals: at {p:?} expected {expected}, got {got}"
                ));
            }
        }
    }

    if let Some(ref m) = spec.array_min_length {
        for (p, min_len) in m {
            let got = v
                .pointer(p)
                .ok_or_else(|| format!("json.arrayMinLength: pointer {p:?} missing in response"))?;
            let arr = got
                .as_array()
                .ok_or_else(|| format!("json.arrayMinLength: pointer {p:?} is not an array"))?;
            if arr.len() < *min_len {
                return Err(format!(
                    "json.arrayMinLength: at {p:?} expected len >= {min_len}, \
                     got {}",
                    arr.len()
                ));
            }
        }
    }

    Ok(())
}

fn assert_sse(spec: &SseAssertions, cap: &CapturedResponse) -> Result<(), String> {
    if let Some(min) = spec.data_frames_min {
        let n = cap.sse_data_line_count();
        if n < min {
            return Err(format!(
                "sse.dataFramesMin: expected >= {min} `data:` lines, got {n}"
            ));
        }
    }
    if spec.has_done == Some(true) && !cap.sse_has_done_marker() {
        return Err("sse.hasDone: expected `[DONE]` marker in body".to_string());
    }
    if spec.has_done == Some(false) && cap.sse_has_done_marker() {
        return Err("sse.hasDone: expected no `[DONE]` marker in body".to_string());
    }
    Ok(())
}

fn assert_headers(spec: &HeadersAssertions, cap: &CapturedResponse) -> Result<(), String> {
    if let Some(ref contains) = spec.contains {
        for (name, expected) in contains {
            let key = name.to_ascii_lowercase();
            let got = cap.headers.get(&key).ok_or_else(|| {
                format!(
                    "headers.contains: missing header {name:?} (lowercase key \
                     {key})"
                )
            })?;
            if got != expected {
                return Err(format!(
                    "headers.contains: {name:?} expected {expected:?}, got \
                     {got:?}"
                ));
            }
        }
    }
    if let Some(ref exists) = spec.exists {
        for name in exists {
            let key = name.to_ascii_lowercase();
            if !cap.headers.contains_key(&key) {
                return Err(format!("headers.exists: missing {name:?}"));
            }
        }
    }
    if let Some(ref not_contains) = spec.not_contains {
        for (name, forbidden) in not_contains {
            let key = name.to_ascii_lowercase();
            if let Some(got) = cap.headers.get(&key)
                && got == forbidden
            {
                return Err(format!(
                    "headers.not_contains: header {name:?} must not be \
                     {forbidden:?}, got {got:?}"
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::case::{AssertionsSpec, BodyAssertions, JsonAssertions, SseAssertions};

    fn sample_cap(status: u16) -> CapturedResponse {
        let mut headers = HashMap::new();
        headers.insert("alephant-cache".to_string(), "HIT".to_string());
        CapturedResponse {
            status,
            headers,
            body: "{}".into(),
            time_total_sec: 0.01,
        }
    }

    #[test]
    fn status_exact_ok() {
        let spec = AssertionsSpec {
            http_status: Some(HttpStatusSpec::Exact(200)),
            ..Default::default()
        };
        assert_response(&spec, &sample_cap(200)).unwrap();
    }

    #[test]
    fn status_exact_fails() {
        let spec = AssertionsSpec {
            http_status: Some(HttpStatusSpec::Exact(200)),
            ..Default::default()
        };
        assert!(assert_response(&spec, &sample_cap(401)).is_err());
    }

    #[test]
    fn header_contains_ok() {
        let mut contains = std::collections::HashMap::new();
        contains.insert("Alephant-Cache".to_string(), "HIT".to_string());
        let spec = AssertionsSpec {
            headers: Some(HeadersAssertions {
                contains: Some(contains),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_response(&spec, &sample_cap(200)).unwrap();
    }

    #[test]
    fn body_contains_ok_and_fails() {
        let cap = CapturedResponse {
            status: 200,
            headers: HashMap::new(),
            body: r#"{"error":{"message":"nope"}}"#.into(),
            time_total_sec: 0.01,
        };
        let ok = AssertionsSpec {
            body: Some(BodyAssertions {
                contains: Some(vec!["error".into(), "nope".into()]),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_response(&ok, &cap).unwrap();

        let bad = AssertionsSpec {
            body: Some(BodyAssertions {
                contains: Some(vec!["missing-xyz".into()]),
                ..Default::default()
            }),
            ..Default::default()
        };
        let err = assert_response(&bad, &cap).unwrap_err();
        assert!(err.contains("missing-xyz"), "{err}");
    }

    #[test]
    fn body_not_contains_fails_when_present() {
        let cap = CapturedResponse {
            status: 200,
            headers: HashMap::new(),
            body: "secret-token".into(),
            time_total_sec: 0.01,
        };
        let spec = AssertionsSpec {
            body: Some(BodyAssertions {
                not_contains: Some(vec!["secret".into()]),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(assert_response(&spec, &cap).is_err());
    }

    #[test]
    fn body_regex_matches() {
        let cap = CapturedResponse {
            status: 200,
            headers: HashMap::new(),
            body: r#"{"id":"chatcmpl-abc"}"#.into(),
            time_total_sec: 0.01,
        };
        let spec = AssertionsSpec {
            body: Some(BodyAssertions {
                regex: Some(r#"chatcmpl-[a-z]+"#.into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_response(&spec, &cap).unwrap();
    }

    #[test]
    fn json_path_exists_and_equals() {
        let cap = CapturedResponse {
            status: 400,
            headers: HashMap::new(),
            body: r#"{"error":{"message":"bad","type":"invalid_request"}}"#.into(),
            time_total_sec: 0.01,
        };
        let mut eq = HashMap::new();
        eq.insert("/error/type".into(), serde_json::json!("invalid_request"));
        let spec = AssertionsSpec {
            json: Some(JsonAssertions {
                path_exists: Some(vec!["/error/message".into()]),
                path_equals: Some(eq),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_response(&spec, &cap).unwrap();
    }

    #[test]
    fn json_array_min_length() {
        let cap = CapturedResponse {
            status: 200,
            headers: HashMap::new(),
            body: r#"{"choices":[{"a":1},{"a":2}]}"#.into(),
            time_total_sec: 0.01,
        };
        let mut m = HashMap::new();
        m.insert("/choices".into(), 2_usize);
        let spec = AssertionsSpec {
            json: Some(JsonAssertions {
                array_min_length: Some(m),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_response(&spec, &cap).unwrap();
    }

    #[test]
    fn sse_data_frames_and_done() {
        let cap = CapturedResponse {
            status: 200,
            headers: HashMap::new(),
            body: "data: {\"x\":1}\n\ndata: [DONE]\n\n".into(),
            time_total_sec: 0.01,
        };
        let spec = AssertionsSpec {
            sse: Some(SseAssertions {
                data_frames_min: Some(2),
                has_done: Some(true),
            }),
            ..Default::default()
        };
        assert_response(&spec, &cap).unwrap();
    }

    #[test]
    fn sse_has_done_false_fails_when_done_present() {
        let cap = CapturedResponse {
            status: 200,
            headers: HashMap::new(),
            body: "data: [DONE]\n".into(),
            time_total_sec: 0.01,
        };
        let spec = AssertionsSpec {
            sse: Some(SseAssertions {
                has_done: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(assert_response(&spec, &cap).is_err());
    }
}
