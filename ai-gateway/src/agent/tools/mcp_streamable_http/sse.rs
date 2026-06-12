#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseLimits {
    pub max_total_bytes: usize,
    pub max_event_bytes: usize,
    pub max_line_bytes: usize,
    pub max_events: usize,
    pub max_batch_items: usize,
}

impl Default for SseLimits {
    fn default() -> Self {
        Self {
            max_total_bytes: 65_536,
            max_event_bytes: 16_384,
            max_line_bytes: 8_192,
            max_events: 256,
            max_batch_items: 64,
        }
    }
}

#[derive(Debug, Default)]
pub struct SseAccumulator {
    buffer: Vec<u8>,
    total_bytes: usize,
    completed_events: usize,
}

impl SseAccumulator {
    pub fn push_and_try_find(
        &mut self,
        chunk: &[u8],
        expected_id: &str,
        limits: &SseLimits,
    ) -> Result<Option<serde_json::Value>, SseParseError> {
        self.buffer.extend_from_slice(chunk);
        validate_buffered_line_lengths(&self.buffer, limits.max_line_bytes)?;

        self.total_bytes = self
            .total_bytes
            .checked_add(chunk.len())
            .ok_or(SseParseError::TotalTooLarge)?;
        if self.total_bytes > limits.max_total_bytes {
            return Err(SseParseError::TotalTooLarge);
        }

        while let Some(split_at) = completed_event_boundary(&self.buffer) {
            let completed = self.buffer[..split_at].to_vec();
            self.buffer = self.buffer[split_at..].to_vec();
            self.completed_events = self
                .completed_events
                .checked_add(1)
                .ok_or(SseParseError::TooManyEvents)?;
            if self.completed_events > limits.max_events {
                return Err(SseParseError::TooManyEvents);
            }
            let text = std::str::from_utf8(&completed).map_err(|_| SseParseError::InvalidUtf8)?;
            match find_json_rpc_response(text, expected_id, limits) {
                Ok(value) => return Ok(Some(value)),
                Err(SseParseError::MatchingResponseMissing) => continue,
                Err(err) => return Err(err),
            }
        }

        Ok(None)
    }
}

fn validate_buffered_line_lengths(
    buffer: &[u8],
    max_line_bytes: usize,
) -> Result<(), SseParseError> {
    let mut line_start = 0_usize;
    for (idx, byte) in buffer.iter().enumerate() {
        if *byte != b'\n' {
            continue;
        }

        let mut line_end = idx;
        if line_end > line_start && buffer[line_end - 1] == b'\r' {
            line_end -= 1;
        }
        if line_end - line_start > max_line_bytes {
            return Err(SseParseError::LineTooLarge);
        }
        line_start = idx + 1;
    }

    if buffer.len() - line_start > max_line_bytes {
        return Err(SseParseError::LineTooLarge);
    }

    Ok(())
}

fn completed_event_boundary(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|idx| idx + 2)
        .or_else(|| {
            buffer
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .map(|idx| idx + 4)
        })
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SseParseError {
    #[error("sse line exceeds limit")]
    LineTooLarge,
    #[error("sse event exceeds limit")]
    EventTooLarge,
    #[error("sse event count exceeds limit")]
    TooManyEvents,
    #[error("sse total bytes exceed limit")]
    TotalTooLarge,
    #[error("sse data is invalid utf-8")]
    InvalidUtf8,
    #[error("json rpc batch exceeds limit")]
    BatchTooLarge,
    #[error("mcp server request is unsupported")]
    ServerRequestUnsupported,
    #[error("matching json rpc response was not found")]
    MatchingResponseMissing,
    #[error("sse data is invalid json")]
    InvalidJson,
}

pub fn find_json_rpc_response(
    sse_text: &str,
    expected_id: &str,
    limits: &SseLimits,
) -> Result<serde_json::Value, SseParseError> {
    let mut event_count = 0_usize;
    let mut data_lines = Vec::new();

    for line in sse_text.lines() {
        if line.len() > limits.max_line_bytes {
            return Err(SseParseError::LineTooLarge);
        }

        if line.is_empty() {
            if !data_lines.is_empty() {
                event_count += 1;
                if event_count > limits.max_events {
                    return Err(SseParseError::TooManyEvents);
                }
            }
            if let Some(value) = process_event(&data_lines, expected_id, limits)? {
                return Ok(value);
            }
            data_lines.clear();
            continue;
        }

        if line.starts_with(':') {
            continue;
        }

        if let Some(data) = line.strip_prefix("data:") {
            data_lines.push(data.trim_start().to_string());
            let size: usize = data_lines.iter().map(String::len).sum();
            if size > limits.max_event_bytes {
                return Err(SseParseError::EventTooLarge);
            }
        }
    }

    if let Some(value) = process_event(&data_lines, expected_id, limits)? {
        return Ok(value);
    }

    Err(SseParseError::MatchingResponseMissing)
}

fn process_event(
    data_lines: &[String],
    expected_id: &str,
    limits: &SseLimits,
) -> Result<Option<serde_json::Value>, SseParseError> {
    if data_lines.is_empty() {
        return Ok(None);
    }

    let data = data_lines.join("\n");
    let value: serde_json::Value =
        serde_json::from_str(&data).map_err(|_| SseParseError::InvalidJson)?;

    if let Some(items) = value.as_array() {
        if items.len() > limits.max_batch_items {
            return Err(SseParseError::BatchTooLarge);
        }

        for item in items {
            if is_server_request(item) {
                return Err(SseParseError::ServerRequestUnsupported);
            }
            if item.get("id") == Some(&serde_json::json!(expected_id)) {
                return Ok(Some(item.clone()));
            }
        }

        return Ok(None);
    }

    if is_server_request(&value) {
        return Err(SseParseError::ServerRequestUnsupported);
    }
    if value.get("id") == Some(&serde_json::json!(expected_id)) {
        return Ok(Some(value));
    }

    Ok(None)
}

fn is_server_request(value: &serde_json::Value) -> bool {
    value.get("method").is_some() && value.get("id").is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_multiline_data_response() {
        let sse = concat!(
            ": keepalive\n",
            "event: message\n",
            "data: {\"jsonrpc\":\"2.0\",\n",
            "data: \"id\":\"exec_1\",\n",
            "data: \"result\":{\"ok\":true}}\n",
            "\n",
        );

        let value = find_json_rpc_response(sse, "exec_1", &SseLimits::default())
            .expect("matching response");

        assert_eq!(value["result"]["ok"], true);
    }

    #[test]
    fn parses_batch_response_and_matches_id() {
        let sse = concat!(
            "data: [",
            "{\"jsonrpc\":\"2.0\",\"id\":\"other\",\"result\":{}},",
            "{\"jsonrpc\":\"2.0\",\"id\":\"exec_1\",\"result\":{\"ok\":true}}",
            "]\n\n"
        );

        let value = find_json_rpc_response(sse, "exec_1", &SseLimits::default())
            .expect("matching response");

        assert_eq!(value["id"], "exec_1");
    }

    #[test]
    fn server_request_fails_in_v1() {
        let sse = concat!(
            "data: {\"jsonrpc\":\"2.0\",\"id\":\"srv_1\",",
            "\"method\":\"sampling/createMessage\",\"params\":{}}\n\n"
        );

        let err = find_json_rpc_response(sse, "exec_1", &SseLimits::default())
            .expect_err("server request unsupported");

        assert_eq!(err, SseParseError::ServerRequestUnsupported);
    }

    #[test]
    fn line_limit_is_enforced() {
        let limits = SseLimits {
            max_line_bytes: 4,
            ..SseLimits::default()
        };

        let err = find_json_rpc_response("data: too-long\n\n", "exec_1", &limits)
            .expect_err("line too large");

        assert_eq!(err, SseParseError::LineTooLarge);
    }

    #[test]
    fn event_limit_is_enforced() {
        let limits = SseLimits {
            max_event_bytes: 8,
            ..SseLimits::default()
        };

        let err = find_json_rpc_response(
            "data: {\"jsonrpc\":\"2.0\",\"id\":\"exec_1\"}\n\n",
            "exec_1",
            &limits,
        )
        .expect_err("event too large");

        assert_eq!(err, SseParseError::EventTooLarge);
    }

    #[test]
    fn event_count_limit_is_enforced() {
        let limits = SseLimits {
            max_events: 1,
            ..SseLimits::default()
        };
        let sse = concat!(
            "data: {\"jsonrpc\":\"2.0\",\"id\":\"other\",\"result\":{}}\n\n",
            "data: {\"jsonrpc\":\"2.0\",\"id\":\"exec_1\",\"result\":{}}\n\n",
        );

        let err = find_json_rpc_response(sse, "exec_1", &limits).expect_err("too many events");

        assert_eq!(err, SseParseError::TooManyEvents);
    }

    #[test]
    fn batch_limit_is_enforced() {
        let limits = SseLimits {
            max_batch_items: 1,
            ..SseLimits::default()
        };
        let sse = concat!(
            "data: [",
            "{\"jsonrpc\":\"2.0\",\"id\":\"other\",\"result\":{}},",
            "{\"jsonrpc\":\"2.0\",\"id\":\"exec_1\",\"result\":{}}",
            "]\n\n"
        );

        let err = find_json_rpc_response(sse, "exec_1", &limits).expect_err("batch too large");

        assert_eq!(err, SseParseError::BatchTooLarge);
    }

    #[test]
    fn incremental_parser_handles_chunk_split_lines() {
        let mut acc = SseAccumulator::default();
        let limits = SseLimits::default();

        assert!(
            acc.push_and_try_find(b"data: {\"jsonrpc\":\"2.0\",", "exec_1", &limits,)
                .unwrap()
                .is_none()
        );
        assert!(
            acc.push_and_try_find(b"\"id\":\"exec_1\"", "exec_1", &limits)
                .unwrap()
                .is_none()
        );
        let value = acc
            .push_and_try_find(b",\"result\":{\"ok\":true}}\n\n", "exec_1", &limits)
            .expect("parse")
            .expect("matching response");

        assert_eq!(value["result"]["ok"], true);
    }

    #[test]
    fn incremental_parser_handles_crlf_event_boundary() {
        let mut acc = SseAccumulator::default();
        let value = acc
            .push_and_try_find(
                b"data: {\"jsonrpc\":\"2.0\",\"id\":\"exec_1\",\"result\":{\"ok\":true}}\r\n\r\n",
                "exec_1",
                &SseLimits::default(),
            )
            .expect("parse")
            .expect("matching response");

        assert_eq!(value["result"]["ok"], true);
    }

    #[test]
    fn incremental_parser_keeps_half_event_without_invalid_json_error() {
        let mut acc = SseAccumulator::default();
        let limits = SseLimits::default();

        assert!(
            acc.push_and_try_find(b"data: {\"jsonrpc\":\"2.0\"", "exec_1", &limits,)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn incremental_parser_enforces_total_limit_on_buffered_bytes() {
        let mut acc = SseAccumulator::default();
        let limits = SseLimits {
            max_total_bytes: 4,
            ..SseLimits::default()
        };

        let err = acc
            .push_and_try_find(b"data:", "exec_1", &limits)
            .expect_err("total too large");

        assert_eq!(err, SseParseError::TotalTooLarge);
    }

    #[test]
    fn incremental_parser_counts_repeated_non_matching_completed_events() {
        let mut acc = SseAccumulator::default();
        let limits = SseLimits {
            max_events: 1,
            ..SseLimits::default()
        };

        assert!(
            acc.push_and_try_find(
                b"data: {\"jsonrpc\":\"2.0\",\"id\":\"other\",\"result\":{}}\n\n",
                "exec_1",
                &limits,
            )
            .expect("first event")
            .is_none()
        );
        let err = acc
            .push_and_try_find(
                b"data: {\"jsonrpc\":\"2.0\",\"id\":\"other_2\",\"result\":{}}\n\n",
                "exec_1",
                &limits,
            )
            .expect_err("too many events");

        assert_eq!(err, SseParseError::TooManyEvents);
    }

    #[test]
    fn incremental_parser_enforces_total_limit_across_completed_events() {
        let mut acc = SseAccumulator::default();
        let limits = SseLimits {
            max_total_bytes: 64,
            ..SseLimits::default()
        };

        assert!(
            acc.push_and_try_find(
                b"data: {\"jsonrpc\":\"2.0\",\"id\":\"other\",\"result\":{}}\n\n",
                "exec_1",
                &limits,
            )
            .expect("first event")
            .is_none()
        );
        let err = acc
            .push_and_try_find(
                b"data: {\"jsonrpc\":\"2.0\",\"id\":\"other_2\",\"result\":{}}\n\n",
                "exec_1",
                &limits,
            )
            .expect_err("total too large");

        assert_eq!(err, SseParseError::TotalTooLarge);
    }

    #[test]
    fn incremental_parser_rejects_unterminated_line_over_limit() {
        let mut acc = SseAccumulator::default();
        let limits = SseLimits {
            max_total_bytes: 1024,
            max_line_bytes: 4,
            ..SseLimits::default()
        };

        let err = acc
            .push_and_try_find(b"data:", "exec_1", &limits)
            .expect_err("line too large");

        assert_eq!(err, SseParseError::LineTooLarge);
    }

    #[test]
    fn incremental_parser_rejects_invalid_utf8_on_completed_event() {
        let mut acc = SseAccumulator::default();
        let err = acc
            .push_and_try_find(&[0xff, b'\n', b'\n'], "exec_1", &SseLimits::default())
            .expect_err("invalid utf8");

        assert_eq!(err, SseParseError::InvalidUtf8);
    }
}
