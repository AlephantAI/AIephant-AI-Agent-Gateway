//! CLI: load `cases/*.json`, run `curl`, assert, optional ClickHouse steps,
//! write `e2e-report.json` (passed) and `e2e-failures.json` (failed).
//!
//! Console stderr: only **failed** cases emit output (`request` body and
//! `response` body). Request/response headers are omitted unless
//! `--header-print` is set. Passing cases and successful full runs are silent.

mod assert;
mod captured;
mod case;
mod ch;
mod curl_run;

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::Context;
use chrono::Utc;
use clap::{Parser, ValueEnum, builder::BoolishValueParser};
use serde::Serialize;

use crate::{
    assert::assert_response,
    captured::CapturedResponse,
    case::{CaseFile, ChPoll, case_matches_profile},
    ch::{ChClient, run_ch_spec},
    curl_run::{
        expand_placeholders, extract_request_body, extract_request_headers,
        inject_request_id_header, redact_expanded_curl, run_curl_shell,
    },
};

#[derive(Clone, Copy, Debug, ValueEnum)]
enum HarnessProfile {
    Gate,
    Full,
}

impl HarnessProfile {
    const fn as_key(self) -> &'static str {
        match self {
            Self::Gate => "gate",
            Self::Full => "full",
        }
    }
}

#[derive(Parser, Debug)]
#[command(name = "gateway-e2e-harness", version)]
struct Cli {
    #[arg(
        long,
        default_value = "crates/gateway-e2e-harness/cases",
        value_name = "DIR"
    )]
    cases_dir: PathBuf,
    #[arg(long, default_value = ".env", value_name = "PATH")]
    dotenv_path: PathBuf,
    #[arg(long, value_enum, default_value_t = HarnessProfile::Gate)]
    profile: HarnessProfile,
    #[arg(long, value_name = "DIR")]
    out_dir: Option<PathBuf>,
    #[arg(long, value_name = "ID")]
    run_id: Option<String>,
    #[arg(long = "var", value_name = "KEY=VAL")]
    vars: Vec<String>,
    /// For failed cases only: print request and response headers (redacted) to
    /// stderr. Omit the flag to skip; use `--header-print` or
    /// `--header-print true` to enable.
    #[arg(
        long = "header-print",
        visible_aliases = ["print-request-headers", "print-headers"],
        num_args = 0..=1,
        default_missing_value = "true",
        value_parser = BoolishValueParser::new()
    )]
    header_print: Option<bool>,
    /// After each authenticated request, query ClickHouse
    /// `request_response_rmt` by the auto-generated `request_id` and include
    /// the row in the report. Requires `CLICKHOUSE_URL` (or
    /// `CLICKHOUSE_HTTP_URL`) to be set.
    #[arg(
        long = "ch-lookup",
        num_args = 0..=1,
        default_missing_value = "true",
        value_parser = BoolishValueParser::new()
    )]
    ch_lookup: Option<bool>,
}

/// One case result; same shape in `e2e-report.json` and `e2e-failures.json`.
#[derive(Serialize)]
struct E2eReportEntry {
    case_id: String,
    /// Auto-generated UUID v7 injected as `X-Request-Id` into the curl
    /// request. `None` when the case was skipped.
    request_id: Option<String>,
    passed: bool,
    /// `true` when the case was not executed because a declared prerequisite
    /// was missing.
    skipped: bool,
    /// `null` when success or skipped; string when failed.
    failure_reason: Option<String>,
    record: serde_json::Value,
}

#[derive(Serialize)]
struct E2eReportDocument {
    schema_version: u32,
    run_id: String,
    profile: String,
    generated_at: String,
    cases: Vec<E2eReportEntry>,
}

fn parse_kv_list(vars: &[String]) -> anyhow::Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    for s in vars {
        let (k, v) = s
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("--var must be KEY=VAL, got {s:?}"))?;
        out.push((k.to_string(), v.to_string()));
    }
    Ok(out)
}

fn missing_required_env(required_env: &[String], vars: &HashMap<String, String>) -> Vec<String> {
    required_env
        .iter()
        .filter(|key| vars.get(*key).is_none_or(|value| value.trim().is_empty()))
        .cloned()
        .collect()
}

fn skipped_required_env_entry(case: &CaseFile, missing_env: Vec<String>) -> E2eReportEntry {
    E2eReportEntry {
        case_id: case.case_id.clone(),
        request_id: None,
        passed: true,
        skipped: true,
        failure_reason: None,
        record: serde_json::json!({
            "case": case,
            "skipped": {
                "reason": "missing required env",
                "missingEnv": missing_env,
            },
        }),
    }
}

fn list_case_files(dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut paths: Vec<PathBuf> = fs::read_dir(dir)
        .with_context(|| format!("read cases_dir {}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    paths.sort();
    Ok(paths)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if let Err(e) = run().await {
        eprintln!("{e:#}");
        std::process::exit(1);
    }
    Ok(())
}

async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    if cli.dotenv_path.exists() {
        dotenvy::from_path(&cli.dotenv_path)
            .with_context(|| format!("load dotenv {}", cli.dotenv_path.display()))?;
    }
    let extra = parse_kv_list(&cli.vars)?;
    let mut vars: HashMap<String, String> = std::env::vars().collect();
    for (k, v) in extra {
        vars.insert(k, v);
    }

    let ch_client = ChClient::from_env()?;

    let run_id = cli
        .run_id
        .clone()
        .unwrap_or_else(|| format!("{}-e2e", Utc::now().format("%Y%m%dT%H%M%SZ")));
    // Default for `$HARNESS_E2E_RUN_TOKEN` in case curl templates (e.g. KV-004)
    // so multi-step cache assertions start from a cold key per run unless
    // pinned via env or `--var`.
    vars.entry("HARNESS_E2E_RUN_TOKEN".to_string())
        .or_insert_with(|| run_id.clone());
    let out_dir = cli.out_dir.clone().unwrap_or_else(|| {
        PathBuf::from("test-artifacts/runs")
            .join(&run_id)
            .join("e2e")
    });
    fs::create_dir_all(&out_dir)
        .with_context(|| format!("create out_dir {}", out_dir.display()))?;
    let report_path = out_dir.join("e2e-report.json");
    let failures_path = out_dir.join("e2e-failures.json");

    let profile_key = cli.profile.as_key();
    let print_failure_headers = cli.header_print.unwrap_or(false);
    let ch_lookup = cli.ch_lookup.unwrap_or(false);
    let mut any_failed = false;
    let mut passed_cases: Vec<E2eReportEntry> = Vec::new();
    let mut failed_cases: Vec<E2eReportEntry> = Vec::new();

    for path in list_case_files(&cli.cases_dir)? {
        let raw = fs::read_to_string(&path)
            .with_context(|| format!("read case file {}", path.display()))?;
        let case: CaseFile = serde_json::from_str(&raw)
            .with_context(|| format!("parse case JSON {}", path.display()))?;
        if !case_matches_profile(&case, profile_key) {
            continue;
        }

        let missing_env = missing_required_env(&case.required_env, &vars);
        if !missing_env.is_empty() {
            passed_cases.push(skipped_required_env_entry(&case, missing_env));
            continue;
        }

        let (passed, failure_reason, request_id, record) =
            run_one_case(&case, &vars, ch_client.as_ref(), ch_lookup).await;
        if !passed {
            any_failed = true;
            eprint_failure_bodies(&case.case_id, &record, &vars, print_failure_headers);
        }
        let entry = E2eReportEntry {
            case_id: case.case_id.clone(),
            request_id: Some(request_id),
            passed,
            skipped: false,
            failure_reason: failure_reason.clone(),
            record,
        };
        if passed {
            passed_cases.push(entry);
        } else {
            failed_cases.push(entry);
        }
    }

    let generated_at = Utc::now().to_rfc3339();
    let doc_passed = E2eReportDocument {
        schema_version: 1,
        run_id: run_id.clone(),
        profile: profile_key.to_string(),
        generated_at: generated_at.clone(),
        cases: passed_cases,
    };
    let doc_failed = E2eReportDocument {
        schema_version: 1,
        run_id,
        profile: profile_key.to_string(),
        generated_at,
        cases: failed_cases,
    };

    fs::write(
        &report_path,
        serde_json::to_string_pretty(&doc_passed)
            .with_context(|| format!("serialize {}", report_path.display()))?,
    )
    .with_context(|| format!("write {}", report_path.display()))?;
    fs::write(
        &failures_path,
        serde_json::to_string_pretty(&doc_failed)
            .with_context(|| format!("serialize {}", failures_path.display()))?,
    )
    .with_context(|| format!("write {}", failures_path.display()))?;

    if any_failed {
        // Reports are already written; per-case details went to stderr.
        // Exit without printing a second summary (keeps stderr to failed cases
        // only).
        std::process::exit(1);
    }
    Ok(())
}

/// Whether the HTTP response indicates successful authentication (i.e. the
/// gateway did **not** reject with 401 or 403).
fn is_authenticated(status: u16) -> bool {
    status != 401 && status != 403
}

async fn run_one_case(
    case: &CaseFile,
    vars: &HashMap<String, String>,
    ch_client: Option<&ChClient>,
    ch_lookup: bool,
) -> (bool, Option<String>, String, serde_json::Value) {
    let mut failure: Option<String> = None;
    let request_id = uuid::Uuid::now_v7().to_string();

    if let Some(ref steps) = case.curl_prelude {
        for (idx, prelude) in steps.iter().enumerate() {
            let expanded = match expand_placeholders(prelude, vars) {
                Ok(s) => s,
                Err(e) => {
                    return (
                        false,
                        Some(format!("expand prelude[{idx}]: {e:#}")),
                        request_id,
                        serde_json::json!({
                            "case": case,
                            "error": format!("{e:#}"),
                            "preludeIndex": idx,
                        }),
                    );
                }
            };
            if let Err(e) = run_curl_shell(&expanded) {
                return (
                    false,
                    Some(format!("curl prelude[{idx}]: {e:#}")),
                    request_id,
                    serde_json::json!({
                        "case": case,
                        "execution": {
                            "curlExpanded": redact_expanded_curl(&expanded, vars),
                            "error": format!("{e:#}"),
                            "preludeIndex": idx,
                        },
                    }),
                );
            }
        }
    }

    let expanded = match expand_placeholders(&case.curl, vars) {
        Ok(s) => s,
        Err(e) => {
            return (
                false,
                Some(format!("expand: {e:#}")),
                request_id,
                serde_json::json!({ "case": case, "error": format!("{e:#}") }),
            );
        }
    };

    let expanded = inject_request_id_header(&expanded, &request_id);

    let captured = match run_curl_shell(&expanded) {
        Ok(c) => c,
        Err(e) => {
            return (
                false,
                Some(format!("curl: {e:#}")),
                request_id,
                serde_json::json!({
                    "case": case,
                    "execution": { "curlExpanded": redact_expanded_curl(&expanded, vars), "error": format!("{e:#}") },
                }),
            );
        }
    };

    if let Err(e) = assert_response(&case.assertions, &captured) {
        failure = Some(format!("assert: {e}"));
    }

    let ch_json = if let Some(ref chspec) = case.ch {
        if failure.is_none() {
            match run_ch_spec(ch_client, chspec, Some(&captured)).await {
                Ok(out) => Some(serde_json::json!({ "rowsSample": out.last_rows })),
                Err(e) => {
                    failure = Some(format!("{e:#}"));
                    None
                }
            }
        } else {
            None
        }
    } else {
        None
    };

    let ch_row = if ch_lookup && is_authenticated(captured.status) {
        if let Some(ch) = ch_client {
            let poll = ChPoll {
                max_attempts: 20,
                backoff_ms: 500,
            };
            match ch.lookup_request_row(&request_id, &poll).await {
                Ok(row) => row,
                Err(e) => {
                    eprintln!(
                        "  [warn] {}: CH lookup for request_id={request_id} \
                         failed: {e:#}",
                        case.case_id
                    );
                    None
                }
            }
        } else {
            None
        }
    } else {
        None
    };

    let passed = failure.is_none();
    let record = serde_json::json!({
        "case": case,
        "execution": {
            "curlExpanded": redact_expanded_curl(&expanded, vars),
            "response": captured_to_json(&captured),
            "ch": ch_json,
            "chRow": ch_row,
        },
    });

    (passed, failure, request_id, record)
}

fn captured_to_json(cap: &CapturedResponse) -> serde_json::Value {
    serde_json::json!({
        "status": cap.status,
        "headers": cap.headers,
        "body": cap.body,
        "timeTotalSec": cap.time_total_sec,
    })
}

/// Failed cases: print request/response bodies (redacted) to stderr; request
/// and response headers only when `print_headers` is true.
fn eprint_failure_bodies(
    case_id: &str,
    record: &serde_json::Value,
    vars: &HashMap<String, String>,
    print_headers: bool,
) {
    let curl_expanded = record
        .get("execution")
        .and_then(|e| e.get("curlExpanded"))
        .and_then(|v| v.as_str());

    let request_body = curl_expanded
        .and_then(extract_request_body)
        .map(|b| redact_expanded_curl(&b, vars))
        .unwrap_or_else(|| "(none)".to_string());

    let response_body = record
        .get("execution")
        .and_then(|e| e.get("response"))
        .and_then(|r| r.get("body"))
        .and_then(|b| b.as_str())
        .unwrap_or("(none)");
    eprintln!("--- {case_id} ---");
    if print_headers {
        let request_headers = curl_expanded
            .map(|c| {
                let hs = extract_request_headers(c);
                if hs.is_empty() {
                    "(none)".to_string()
                } else {
                    redact_expanded_curl(&hs.join("\n"), vars)
                }
            })
            .unwrap_or_else(|| "(none)".to_string());
        eprintln!("request headers:\n{request_headers}");
    }
    eprintln!("request body:\n{request_body}");
    if print_headers {
        let response_headers = record
            .get("execution")
            .and_then(|e| e.get("response"))
            .and_then(|r| r.get("headers"))
            .map(|h| {
                let s = format_json_headers_multiline(h);
                if s == "(none)" {
                    s
                } else {
                    redact_expanded_curl(&s, vars)
                }
            })
            .unwrap_or_else(|| "(none)".to_string());
        eprintln!("response headers:\n{response_headers}");
    }
    eprintln!("response body:\n{response_body}");
}

/// One `name: value` per line; sorted by header name for stable output.
fn format_json_headers_multiline(headers: &serde_json::Value) -> String {
    let Some(obj) = headers.as_object() else {
        return "(none)".to_string();
    };
    let mut keys: Vec<&String> = obj.keys().collect();
    keys.sort();
    let mut lines = Vec::new();
    for k in keys {
        let v = obj
            .get(k)
            .and_then(|v| v.as_str())
            .unwrap_or("<non-string>");
        lines.push(format!("{k}: {v}"));
    }
    if lines.is_empty() {
        "(none)".to_string()
    } else {
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::case::{AssertionsSpec, Tier};

    fn sample_case() -> CaseFile {
        CaseFile {
            case_id: "env1".to_string(),
            tier: Tier::L2,
            curl_prelude: None,
            curl: "curl http://example.com".to_string(),
            intent: "i".to_string(),
            required_env: vec![],
            assertions: AssertionsSpec::default(),
            ch: None,
            profile: None,
        }
    }

    #[test]
    fn missing_required_env_treats_absent_and_blank_as_missing() {
        let required_env = vec![
            "MISSING".to_string(),
            "BLANK".to_string(),
            "PRESENT".to_string(),
        ];
        let vars = HashMap::from([
            ("BLANK".to_string(), " \t\n".to_string()),
            ("PRESENT".to_string(), "value".to_string()),
        ]);

        assert_eq!(
            missing_required_env(&required_env, &vars),
            vec!["MISSING".to_string(), "BLANK".to_string()]
        );
    }

    #[test]
    fn skipped_required_env_entry_has_expected_report_shape() {
        let case = sample_case();
        let entry = skipped_required_env_entry(&case, vec!["OPENAI_API_KEY".to_string()]);

        assert_eq!(entry.case_id, "env1");
        assert!(entry.passed);
        assert!(entry.skipped);
        assert!(entry.failure_reason.is_none());
        assert_eq!(
            entry.record.pointer("/skipped/reason").unwrap(),
            "missing required env"
        );
        assert_eq!(
            entry.record.pointer("/skipped/missingEnv").unwrap(),
            &serde_json::json!(["OPENAI_API_KEY"])
        );
    }

    #[test]
    fn normal_report_entry_serializes_skipped_false() {
        let entry = E2eReportEntry {
            case_id: "ok1".to_string(),
            request_id: Some("test-rid".to_string()),
            passed: true,
            skipped: false,
            failure_reason: None,
            record: serde_json::json!({"caseId": "ok1"}),
        };

        let value = serde_json::to_value(entry).unwrap();
        assert_eq!(value["case_id"], "ok1");
        assert_eq!(value["request_id"], "test-rid");
        assert_eq!(value["passed"], true);
        assert_eq!(value["skipped"], false);
        assert!(value["failure_reason"].is_null());
    }
}
