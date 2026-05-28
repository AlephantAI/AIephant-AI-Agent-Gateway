//! Expand `$VAR` placeholders and run `curl` via `sh -c`.

use std::{collections::HashMap, process::Command};

use regex::Regex;

use crate::captured::CapturedResponse;

/// Expand `$UPPER_SNAKE` placeholders using `vars`. Missing key → error.
pub fn expand_placeholders(
    template: &str,
    vars: &HashMap<String, String>,
) -> anyhow::Result<String> {
    let re =
        Regex::new(r"\$([A-Z][A-Z0-9_]*)").map_err(|e| anyhow::anyhow!("regex compile: {e}"))?;
    let mut out = String::new();
    let mut last = 0;
    for cap in re.captures_iter(template) {
        let m = cap.get(0).unwrap();
        out.push_str(&template[last..m.start()]);
        let key = cap.get(1).unwrap().as_str();
        let val = vars.get(key).ok_or_else(|| {
            anyhow::anyhow!("missing substitution for ${key} (set in .env or CLI)")
        })?;
        out.push_str(val);
        last = m.end();
    }
    out.push_str(&template[last..]);
    Ok(out)
}

/// Inject `-H "X-Request-Id: <uuid>"` after the leading `curl` token so the
/// gateway uses a harness-supplied request id (instead of generating its own).
#[must_use]
pub fn inject_request_id_header(expanded: &str, request_id: &str) -> String {
    let t = expanded.trim_start();
    if let Some(rest) = t.strip_prefix("curl") {
        format!(
            "curl -H \"X-Request-Id: {request_id}\" {}",
            rest.trim_start()
        )
    } else {
        expanded.to_string()
    }
}

/// Inject `-sS -D -` and `-w` trailer after the leading `curl` token.
pub fn inject_curl_trailer(expanded: &str) -> anyhow::Result<String> {
    let t = expanded.trim_start();
    if let Some(rest) = t.strip_prefix("curl") {
        // rest may start with space or tab
        let rest = rest.trim_start();
        let w =
            r#"-sS -D - -w '\n__HARNESS_STATUS__:%{http_code}\n__HARNESS_TIME__:%{time_total}\n'"#;
        Ok(format!("curl {w} {rest}"))
    } else {
        anyhow::bail!("case `curl` must start with `curl` (after leading whitespace)");
    }
}

/// Run expanded curl command through `sh -c` and parse stdout.
pub fn run_curl_shell(expanded: &str) -> anyhow::Result<CapturedResponse> {
    let cmd = inject_curl_trailer(expanded)?;
    let output = Command::new("sh")
        .arg("-c")
        .arg(&cmd)
        .output()
        .map_err(|e| anyhow::anyhow!("failed to spawn sh -c curl: {e}"))?;
    let _stderr = String::from_utf8_lossy(&output.stderr);
    let raw = String::from_utf8_lossy(&output.stdout).into_owned();
    if !output.status.success() {
        anyhow::bail!(
            "curl exited {:?}: stderr={} stdout_prefix={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr),
            raw.chars().take(400).collect::<String>()
        );
    }
    CapturedResponse::parse_curl_stdout(&raw)
}

/// Best-effort extract of `-H` / `--header` values from an expanded `curl`
/// command (`Name: value` lines). Order matches appearance in the command.
#[must_use]
pub fn extract_request_headers(expanded: &str) -> Vec<String> {
    let Ok(re) = Regex::new(
        r#"(?:^|[\s])(?:-H|--header)(?:=|\s*)("(?:\\.|[^"\\])*"|'(?:\\.|[^'])*'|[^\s]+)"#,
    ) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for cap in re.captures_iter(expanded) {
        let Some(m) = cap.get(1) else {
            continue;
        };
        let arg = m.as_str();
        let line = if let Some(inner) = arg.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
            unescape_curl_double_quoted(inner)
        } else if let Some(inner) = arg.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
            inner.replace("\\'", "'")
        } else {
            arg.to_string()
        };
        out.push(line);
    }
    out
}

fn unescape_curl_double_quoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(n) = chars.next() {
                out.push(n);
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Best-effort extract of HTTP request body from an expanded `curl` command
/// (`-d`, `--data`, `--data-binary`, `--data-raw`). Returns `None` when no
/// data flag is present or parsing fails.
#[must_use]
pub fn extract_request_body(expanded: &str) -> Option<String> {
    let re = Regex::new(r#"(?:^|[\s])(-d|--data-binary|--data-raw|--data)(?:=|\s+)"#).ok()?;
    let m = re.find(expanded)?;
    let mut rest = &expanded[m.end()..];
    if rest.starts_with('=') {
        rest = &rest[1..];
    }
    rest = rest.trim_start();

    if let Some(after) = rest.strip_prefix("\\'") {
        let (body, _) = after.split_once("\\'")?;
        return Some(body.to_string());
    }
    if let Some(after) = rest.strip_prefix('\'') {
        let (body, _) = after.split_once('\'')?;
        return Some(body.to_string());
    }
    if let Some(rest) = rest.strip_prefix('"') {
        let mut out = String::new();
        let mut chars = rest.chars();
        while let Some(c) = chars.next() {
            if c == '"' {
                return Some(out);
            }
            if c == '\\' {
                out.push(chars.next()?);
            } else {
                out.push(c);
            }
        }
        return None;
    }
    if rest.starts_with('@') {
        let end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
        return Some(rest[..end].to_string());
    }
    let end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
    if end == 0 {
        None
    } else {
        Some(rest[..end].to_string())
    }
}

/// Redact secret material from the expanded curl line using env `vars` (keys
/// matching `KEY`, `TOKEN`, `SECRET`, or suffix `_VK`).
#[must_use]
pub fn redact_expanded_curl(s: &str, vars: &HashMap<String, String>) -> String {
    let mut out = s.to_string();
    for (k, v) in vars {
        let sensitive =
            k.contains("KEY") || k.ends_with("_VK") || k.contains("TOKEN") || k.contains("SECRET");
        if sensitive && v.len() > 4 {
            out = out.replace(v.as_str(), "<redacted>");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_replaces_vars() {
        let mut m = HashMap::new();
        m.insert("GATEWAY_BASE".into(), "http://localhost:9999".into());
        let s = expand_placeholders("curl $GATEWAY_BASE/x", &m).unwrap();
        assert_eq!(s, "curl http://localhost:9999/x");
    }

    #[test]
    fn inject_trailer_prefix() {
        let s = inject_curl_trailer("curl http://example.com").unwrap();
        assert!(s.contains("__HARNESS_STATUS__"));
        assert!(s.contains("-D -"));
    }

    #[test]
    fn extract_body_single_quotes() {
        let s = r#"curl -X POST http://x -H "Content-Type: application/json" -d '{"a":1}'"#;
        assert_eq!(extract_request_body(s).as_deref(), Some(r#"{"a":1}"#));
    }

    #[test]
    fn extract_body_escaped_shell_quotes() {
        let s = "curl -X POST http://x -d \\'{\"model\":\"m\"}\\'";
        assert_eq!(extract_request_body(s).as_deref(), Some(r#"{"model":"m"}"#));
    }

    #[test]
    fn extract_body_double_quotes() {
        let s = r#"curl http://x -d "{\"x\":1}""#;
        assert_eq!(extract_request_body(s).as_deref(), Some(r#"{"x":1}"#));
    }

    #[test]
    fn extract_headers_double_quoted() {
        let s = r#"curl http://x -H "Authorization: Bearer z" -H "Content-Type: application/json" -d '{}'"#;
        let h = extract_request_headers(s);
        assert_eq!(
            h,
            vec![
                "Authorization: Bearer z".to_string(),
                "Content-Type: application/json".to_string(),
            ]
        );
    }

    #[test]
    fn extract_headers_long_form() {
        let s = "curl http://x --header 'X-Test: 1' -d ''";
        assert_eq!(extract_request_headers(s), vec!["X-Test: 1".to_string()]);
    }
}
