//! JSON case file schema (`cases/*.json`).

use serde::{Deserialize, Serialize};

/// Top-level case file deserialized from JSON.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaseFile {
    pub case_id: String,
    pub tier: Tier,
    /// Optional `curl` lines run **before** the main [`Self::curl`], in order
    /// (e.g. warm a rate-limit window). Each string must start with `curl`
    /// after expansion; responses are discarded.
    #[serde(default)]
    pub curl_prelude: Option<Vec<String>>,
    pub curl: String,
    pub intent: String,
    /// Environment variables required to run this case. Missing variables skip
    /// the case instead of failing it, which keeps secret- or
    /// service-dependent probes out of default failure reports.
    #[serde(default)]
    pub required_env: Vec<String>,
    #[serde(default)]
    pub assertions: AssertionsSpec,
    pub ch: Option<ChSpec>,
    /// Tags such as `gate` or `full`. When omitted, the case runs for every
    /// CLI profile.
    #[serde(default)]
    pub profile: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub enum Tier {
    #[serde(rename = "L0")]
    L0,
    #[serde(rename = "L1")]
    L1,
    #[serde(rename = "L2")]
    L2,
    #[serde(rename = "L3")]
    L3,
    #[serde(rename = "L4")]
    L4,
    #[serde(rename = "L5")]
    L5,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct AssertionsSpec {
    pub http_status: Option<HttpStatusSpec>,
    pub headers: Option<HeadersAssertions>,
    /// Plain-text / substring assertions on the response body.
    #[serde(default)]
    pub body: Option<BodyAssertions>,
    /// Assertions on JSON parsed from the body (RFC 6901 JSON Pointers).
    #[serde(default)]
    pub json: Option<JsonAssertions>,
    /// Assertions for OpenAI-style SSE (`data:` lines, `[DONE]`).
    #[serde(default)]
    pub sse: Option<SseAssertions>,
}

/// Substring and optional regex match on `CapturedResponse.body`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct BodyAssertions {
    /// Each substring must appear in the body.
    #[serde(default)]
    pub contains: Option<Vec<String>>,
    /// None of these substrings may appear in the body.
    #[serde(default)]
    pub not_contains: Option<Vec<String>>,
    /// If set, the regex must match somewhere in the body (Rust `regex` crate
    /// semantics).
    #[serde(default)]
    pub regex: Option<String>,
}

/// JSON body assertions using [`serde_json::Value::pointer`].
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct JsonAssertions {
    /// Each JSON Pointer must exist (resolve to a value, including `null`).
    #[serde(default)]
    pub path_exists: Option<Vec<String>>,
    /// Map JSON Pointer → expected JSON value (deep equality).
    #[serde(default)]
    pub path_equals: Option<std::collections::HashMap<String, serde_json::Value>>,
    /// Map JSON Pointer → minimum array length at that path.
    #[serde(default)]
    pub array_min_length: Option<std::collections::HashMap<String, usize>>,
}

/// SSE stream assertions (best-effort; body is the full streamed payload).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct SseAssertions {
    /// Minimum number of lines that start with `data:` (after leading trim).
    #[serde(default)]
    pub data_frames_min: Option<usize>,
    /// If `true`, body must contain the `[DONE]` stream terminator.
    #[serde(default)]
    pub has_done: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum HttpStatusSpec {
    Exact(u16),
    In {
        #[serde(rename = "in")]
        in_list: Vec<u16>,
    },
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct HeadersAssertions {
    #[serde(default)]
    pub contains: Option<std::collections::HashMap<String, String>>,
    #[serde(default)]
    pub exists: Option<Vec<String>>,
    /// Fail if any listed header is present with exactly this value (ASCII
    /// header names).
    #[serde(default, alias = "not_contains")]
    pub not_contains: Option<std::collections::HashMap<String, String>>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChSpec {
    #[serde(default)]
    pub poll: Option<ChPoll>,
    /// When set, each `clickhouse_query` must be immediately followed by
    /// `assert_row`. After a successful query, if `assert_row` fails, the
    /// harness re-runs the same expanded SQL until the assertion passes or
    /// these limits are hit. Omitted => single assert attempt (current
    /// behavior).
    #[serde(default)]
    pub assert_poll: Option<ChPoll>,
    pub steps: Vec<ChStep>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChPoll {
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
    #[serde(default = "default_backoff_ms")]
    pub backoff_ms: u64,
}

impl Default for ChPoll {
    fn default() -> Self {
        Self {
            max_attempts: default_max_attempts(),
            backoff_ms: default_backoff_ms(),
        }
    }
}

fn default_max_attempts() -> u32 {
    30
}

fn default_backoff_ms() -> u64 {
    500
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ChStep {
    Sleep {
        ms: u64,
    },
    ClickhouseQuery {
        sql: String,
    },
    AssertRow {
        #[serde(rename = "match")]
        equals: serde_json::Map<String, serde_json::Value>,
    },
}

/// Whether a case should run under the given CLI profile (`gate` or `full`).
#[must_use]
pub fn case_matches_profile(case: &CaseFile, profile: &str) -> bool {
    let tags = case.profile.as_deref().unwrap_or_default();
    if tags.is_empty() {
        return true;
    }
    match profile {
        "gate" => tags.iter().any(|t| t == "gate"),
        "full" => tags.iter().any(|t| t == "full"),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_tier() {
        let j = r#"{"caseId":"x","tier":"LX","curl":"curl http://example.com","intent":"t"}"#;
        assert!(serde_json::from_str::<CaseFile>(j).is_err());
    }

    #[test]
    fn parses_minimal_case() {
        let j = r#"{"caseId":"x","tier":"L1","curl":"curl http://example.com","intent":"t"}"#;
        let c: CaseFile = serde_json::from_str(j).unwrap();
        assert_eq!(c.case_id, "x");
        assert_eq!(c.tier, Tier::L1);
        assert!(c.curl_prelude.is_none());
    }

    #[test]
    fn parses_curl_prelude() {
        let j = r#"{"caseId":"p","tier":"L2","curlPrelude":["curl http://a"],"curl":"curl http://b","intent":"i"}"#;
        let c: CaseFile = serde_json::from_str(j).unwrap();
        assert_eq!(
            c.curl_prelude.as_ref().unwrap(),
            &["curl http://a".to_string()]
        );
    }

    #[test]
    fn parses_required_env() {
        let j = r#"{
        "caseId":"env1",
        "tier":"L2",
        "curl":"curl http://example.com",
        "intent":"i",
        "requiredEnv":["OPENAI_API_KEY","EMBEDDING_MODEL_NAME"]
    }"#;
        let c: CaseFile = serde_json::from_str(j).unwrap();
        assert_eq!(
            c.required_env,
            vec![
                "OPENAI_API_KEY".to_string(),
                "EMBEDDING_MODEL_NAME".to_string()
            ]
        );
    }

    #[test]
    fn profile_gate_skips_full_only() {
        let c: CaseFile = serde_json::from_str(
            r#"{"caseId":"a","tier":"L2","curl":"curl x","intent":"i","profile":["full"]}"#,
        )
        .unwrap();
        assert!(!case_matches_profile(&c, "gate"));
        assert!(case_matches_profile(&c, "full"));
    }

    #[test]
    fn profile_full_skips_gate_only() {
        let c: CaseFile = serde_json::from_str(
            r#"{"caseId":"b","tier":"L1","curl":"curl x","intent":"i","profile":["gate"]}"#,
        )
        .unwrap();
        assert!(case_matches_profile(&c, "gate"));
        assert!(!case_matches_profile(&c, "full"));
    }

    #[test]
    fn all_repo_case_json_files_deserialize() {
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let cases_dir = manifest_dir.join("cases");
        let mut paths: Vec<_> = std::fs::read_dir(&cases_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect();
        paths.sort();
        assert!(!paths.is_empty(), "no json under {}", cases_dir.display());
        for p in paths {
            let raw = std::fs::read_to_string(&p).unwrap();
            serde_json::from_str::<CaseFile>(&raw).unwrap_or_else(|e| {
                panic!("deserialize {}: {e}", p.display());
            });
        }
    }

    #[test]
    fn parses_ch_with_assert_poll_empty_defaults() {
        let j = r#"{
        "caseId":"ch1","tier":"L2","curl":"curl http://x","intent":"i",
        "ch":{"assertPoll":{},"steps":[
            {"type":"clickhouse_query","sql":"SELECT 1"},
            {"type":"assert_row","match":{"a":"1"}}
        ]}
    }"#;
        let c: CaseFile = serde_json::from_str(j).unwrap();
        let ch = c.ch.as_ref().unwrap();
        assert!(ch.assert_poll.is_some());
        let ap = ch.assert_poll.as_ref().unwrap();
        assert_eq!(ap.max_attempts, 30);
        assert_eq!(ap.backoff_ms, 500);
    }
}
