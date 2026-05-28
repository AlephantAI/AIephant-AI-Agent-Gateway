//! Parsed HTTP response from `curl` stdout (`-D -` + body + `-w` trailer).

use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct CapturedResponse {
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub body: String,
    pub time_total_sec: f64,
}

impl CapturedResponse {
    /// Count SSE `data:` lines (trimmed line starts with `data:`).
    #[must_use]
    pub fn sse_data_line_count(&self) -> usize {
        self.body
            .lines()
            .filter(|line| line.trim_start().starts_with("data:"))
            .count()
    }

    /// True if the body contains an OpenAI-style stream end (`[DONE]`).
    #[must_use]
    pub fn sse_has_done_marker(&self) -> bool {
        self.body.contains("[DONE]")
    }

    /// Parse `curl -D -` stdout: RFC 9112 style header block, body, then
    /// harness trailer lines.
    ///
    /// Trailer format (last non-empty lines of stdout):
    /// `__HARNESS_STATUS__:<code>`
    /// `__HARNESS_TIME__:<seconds>`
    pub fn parse_curl_stdout(raw: &str) -> anyhow::Result<Self> {
        let marker = "__HARNESS_STATUS__:";
        let idx = raw
            .rfind(marker)
            .ok_or_else(|| anyhow::anyhow!("missing __HARNESS_STATUS__ trailer; check curl -w"))?;
        let prefix = raw[..idx].trim_end();
        let trailer = &raw[idx..];

        let mut status: Option<u16> = None;
        let mut time_total: Option<f64> = None;
        for line in trailer.lines() {
            if let Some(v) = line.strip_prefix("__HARNESS_STATUS__:") {
                status = Some(v.trim().parse()?);
            } else if let Some(v) = line.strip_prefix("__HARNESS_TIME__:") {
                time_total = Some(v.trim().parse()?);
            }
        }
        let status = status.ok_or_else(|| anyhow::anyhow!("trailer missing status"))?;
        let time_total_sec =
            time_total.ok_or_else(|| anyhow::anyhow!("trailer missing time_total"))?;

        let (hdr_block, body) = split_headers_body(prefix);

        let headers = parse_headers_block(hdr_block)?;

        Ok(Self {
            status,
            headers,
            body: body.to_string(),
            time_total_sec,
        })
    }
}

fn split_headers_body(prefix: &str) -> (&str, &str) {
    const CRLF2: &str = "\r\n\r\n";
    if let Some(i) = prefix.find(CRLF2) {
        let (hdr, rest) = prefix.split_at(i);
        (hdr, rest.strip_prefix(CRLF2).unwrap_or(rest))
    } else if let Some(i) = prefix.find("\n\n") {
        let (hdr, rest) = prefix.split_at(i);
        (hdr, rest.strip_prefix("\n\n").unwrap_or(rest))
    } else {
        ("", prefix)
    }
}

fn parse_headers_block(hdr_block: &str) -> anyhow::Result<HashMap<String, String>> {
    let mut m = HashMap::new();
    let mut lines = hdr_block.lines();
    let status_line = lines.next().unwrap_or("");
    if !status_line.starts_with("HTTP/") {
        // curl may print nothing before body on some errors; tolerate empty map
        return Ok(m);
    }
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            m.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    Ok(m)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn parses_status_200() {
        let raw = concat!(
            "HTTP/1.1 200 OK\r\n",
            "Content-Type: application/json\r\n",
            "\r\n",
            "{\"ok\":true}\n",
            "__HARNESS_STATUS__:200\n",
            "__HARNESS_TIME__:0.042\n",
        );
        let c = CapturedResponse::parse_curl_stdout(raw).unwrap();
        assert_eq!(c.status, 200);
        assert_eq!(c.body.trim(), "{\"ok\":true}");
        assert!((c.time_total_sec - 0.042).abs() < 1e-9);
        assert_eq!(
            c.headers.get("content-type").map(String::as_str),
            Some("application/json")
        );
    }

    #[test]
    fn sse_helpers_count_data_and_done() {
        let c = CapturedResponse {
            status: 200,
            headers: HashMap::new(),
            body: "data: {\"x\":1}\n\ndata: [DONE]\n".into(),
            time_total_sec: 0.01,
        };
        assert_eq!(c.sse_data_line_count(), 2);
        assert!(c.sse_has_done_marker());
    }
}
