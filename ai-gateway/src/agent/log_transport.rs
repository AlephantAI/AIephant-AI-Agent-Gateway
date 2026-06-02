use std::{sync::Arc, time::Duration};

use reqwest::{
    Client, StatusCode,
    header::{CONTENT_TYPE, HeaderName},
};
use url::Url;

use crate::{
    agent::log_payload::AgentEventLogPayload, app_redis::AppRedis,
    types::secret::Secret,
};

#[derive(Clone, Debug)]
pub struct AgentEventLogTransport {
    redis: Option<Arc<AppRedis>>,
    stream_key: String,
    http_fallback_enabled: bool,
    http_endpoint: Url,
    http_timeout: Duration,
    http_auth_header: String,
    http_auth_token: Secret<String>,
    http_client: Client,
}

#[derive(Debug, thiserror::Error)]
pub enum AgentEventLogTransportError {
    #[error("failed to serialize agent event log payload")]
    Serialize(#[from] serde_json::Error),
    #[error("agent event log redis error")]
    Redis(redis::RedisError),
    #[error("agent event log redis {0} timed out")]
    RedisTimeout(&'static str),
    #[error("failed to send agent event log over HTTP")]
    Http(#[from] reqwest::Error),
    #[error("agent event log HTTP fallback returned status {0}")]
    HttpStatus(StatusCode),
    #[error("agent event log HTTP fallback is disabled")]
    HttpFallbackDisabled,
    #[error("invalid agent event log HTTP auth header name: {0}")]
    InvalidHeaderName(String),
}

impl AgentEventLogTransport {
    #[must_use]
    pub fn new(
        redis: Option<Arc<AppRedis>>,
        stream_key: String,
        http_fallback_enabled: bool,
        http_endpoint: Url,
        http_timeout: Duration,
        http_auth_header: String,
        http_auth_token: String,
        http_client: Client,
    ) -> Self {
        Self {
            redis,
            stream_key,
            http_fallback_enabled,
            http_endpoint,
            http_timeout,
            http_auth_header,
            http_auth_token: Secret::from(http_auth_token),
            http_client,
        }
    }

    pub async fn send(
        &self,
        payload: &AgentEventLogPayload,
    ) -> Result<(), AgentEventLogTransportError> {
        let body = serde_json::to_string(payload)?;

        let mut redis_error = None;
        if let Some(redis) = &self.redis {
            match tokio::time::timeout(self.http_timeout, redis.ping()).await {
                Ok(Ok(())) => {
                    match tokio::time::timeout(
                        self.http_timeout,
                        redis.xadd_payload(&self.stream_key, &body),
                    )
                    .await
                    {
                        Ok(Ok(())) => return Ok(()),
                        Ok(Err(err)) => {
                            if self.http_fallback_enabled {
                                tracing::warn!(
                                    error = %err,
                                    "agent event log Redis XADD failed; attempting HTTP fallback"
                                );
                            } else {
                                tracing::warn!(
                                    error = %err,
                                    "agent event log Redis XADD failed; HTTP fallback disabled"
                                );
                            }
                            redis_error = Some(err);
                        }
                        Err(_elapsed) => {
                            tracing::warn!(
                                "agent event log Redis XADD timed out; \
                                 attempting HTTP fallback"
                            );
                            return self
                                .send_http(
                                    body,
                                    Some(AgentEventLogTransportError::RedisTimeout(
                                        "xadd",
                                    )),
                                )
                                .await;
                        }
                    }
                }
                Ok(Err(err)) => {
                    if self.http_fallback_enabled {
                        tracing::warn!(
                            error = %err,
                            "agent event log Redis ping failed; attempting HTTP fallback"
                        );
                    } else {
                        tracing::warn!(
                            error = %err,
                            "agent event log Redis ping failed; HTTP fallback disabled"
                        );
                    }
                    redis_error = Some(err);
                }
                Err(_elapsed) => {
                    tracing::warn!(
                        "agent event log Redis ping timed out; attempting \
                         HTTP fallback"
                    );
                    return self
                        .send_http(
                            body,
                            Some(AgentEventLogTransportError::RedisTimeout(
                                "ping",
                            )),
                        )
                        .await;
                }
            }
        }

        self.send_http(
            body,
            redis_error.map(AgentEventLogTransportError::Redis),
        )
        .await
    }

    async fn send_http(
        &self,
        body: String,
        redis_error: Option<AgentEventLogTransportError>,
    ) -> Result<(), AgentEventLogTransportError> {
        if !self.http_fallback_enabled {
            if let Some(err) = redis_error {
                return Err(err);
            }
            return Err(AgentEventLogTransportError::HttpFallbackDisabled);
        }

        let mut request = self
            .http_client
            .post(self.http_endpoint.clone())
            .timeout(self.http_timeout)
            .header(CONTENT_TYPE, "application/json")
            .body(body);

        let http_auth_token = self.http_auth_token.expose().trim();
        if !http_auth_token.is_empty() {
            let header_name =
                HeaderName::from_bytes(self.http_auth_header.as_bytes())
                    .map_err(|_| {
                        AgentEventLogTransportError::InvalidHeaderName(
                            self.http_auth_header.clone(),
                        )
                    })?;
            let header_value =
                if header_name.as_str().eq_ignore_ascii_case("authorization") {
                    format!("Bearer {http_auth_token}")
                } else {
                    http_auth_token.to_string()
                };
            request = request.header(header_name, header_value);
        }

        let response = request.send().await?;
        let status = response.status();
        if !status.is_success() {
            return Err(AgentEventLogTransportError::HttpStatus(status));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::{TcpListener, TcpStream},
        sync::{Arc, mpsc},
        thread,
        time::Duration,
    };

    use serde_json::json;
    use url::Url;

    use super::{AgentEventLogTransport, AgentEventLogTransportError};
    use crate::{
        agent::log_payload::AgentEventLogPayload, app_redis::AppRedis,
    };

    #[test]
    fn debug_does_not_expose_http_auth_token() {
        let transport = AgentEventLogTransport::new(
            None,
            "agent-events".to_string(),
            true,
            Url::parse("http://127.0.0.1:1/v1/log/agent-event").unwrap(),
            Duration::from_secs(1),
            "authorization".to_string(),
            "agent-token".to_string(),
            reqwest::Client::new(),
        );

        let debug = format!("{transport:?}");

        assert!(!debug.contains("agent-token"));
        assert!(debug.contains("*****"));
    }

    #[tokio::test]
    async fn falls_back_to_http_when_redis_missing_with_bearer_auth() {
        let fixture = HttpFixture::start(200);
        let transport = AgentEventLogTransport::new(
            None,
            "agent-events".to_string(),
            true,
            fixture.url(),
            Duration::from_secs(1),
            "authorization".to_string(),
            "agent-token".to_string(),
            reqwest::Client::new(),
        );

        transport.send(&payload_fixture()).await.unwrap();

        let request = fixture.receive();
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/v1/log/agent-event");
        assert_eq!(request.header("authorization"), Some("Bearer agent-token"));
        assert!(request.body.contains("\"eventId\":\"evt-1\""));
    }

    #[tokio::test]
    async fn whitespace_only_auth_token_omits_auth_header() {
        let fixture = HttpFixture::start(200);
        let transport = AgentEventLogTransport::new(
            None,
            "agent-events".to_string(),
            true,
            fixture.url(),
            Duration::from_secs(1),
            "authorization".to_string(),
            "   ".to_string(),
            reqwest::Client::new(),
        );

        transport.send(&payload_fixture()).await.unwrap();

        let request = fixture.receive();
        assert_eq!(request.header("authorization"), None);
    }

    #[tokio::test]
    async fn returns_http_status_when_fallback_response_is_not_successful() {
        let fixture = HttpFixture::start(500);
        let transport = AgentEventLogTransport::new(
            None,
            "agent-events".to_string(),
            true,
            fixture.url(),
            Duration::from_secs(1),
            "authorization".to_string(),
            "agent-token".to_string(),
            reqwest::Client::new(),
        );

        let err = transport.send(&payload_fixture()).await.unwrap_err();

        assert!(matches!(
            err,
            AgentEventLogTransportError::HttpStatus(
                reqwest::StatusCode::INTERNAL_SERVER_ERROR
            )
        ));
    }

    #[tokio::test]
    async fn returns_fallback_disabled_when_redis_missing_and_http_disabled() {
        let transport = AgentEventLogTransport::new(
            None,
            "agent-events".to_string(),
            false,
            Url::parse("http://127.0.0.1:1/v1/log/agent-event").unwrap(),
            Duration::from_secs(1),
            "authorization".to_string(),
            "agent-token".to_string(),
            reqwest::Client::new(),
        );

        let err = transport.send(&payload_fixture()).await.unwrap_err();

        assert!(matches!(
            err,
            AgentEventLogTransportError::HttpFallbackDisabled
        ));
    }

    #[tokio::test]
    async fn returns_redis_error_when_redis_fails_and_fallback_disabled() {
        let transport = AgentEventLogTransport::new(
            Some(Arc::new(AppRedis::new(
                Url::parse("redis://127.0.0.1:1").unwrap(),
            ))),
            "agent-events".to_string(),
            false,
            Url::parse("http://127.0.0.1:1/v1/log/agent-event").unwrap(),
            Duration::from_millis(100),
            "authorization".to_string(),
            "agent-token".to_string(),
            reqwest::Client::new(),
        );

        let err = tokio::time::timeout(
            Duration::from_secs(2),
            transport.send(&payload_fixture()),
        )
        .await
        .unwrap()
        .unwrap_err();

        assert!(matches!(err, AgentEventLogTransportError::Redis(_)));
    }

    #[tokio::test]
    async fn falls_back_to_http_when_redis_xadd_fails() {
        let redis = RedisFixture::start(RedisMode::XaddError);
        let http = HttpFixture::start(200);
        let transport = AgentEventLogTransport::new(
            Some(Arc::new(AppRedis::new(redis.url()))),
            "agent-events".to_string(),
            true,
            http.url(),
            Duration::from_secs(1),
            "authorization".to_string(),
            "agent-token".to_string(),
            reqwest::Client::new(),
        );

        transport.send(&payload_fixture()).await.unwrap();

        let request = http.receive();
        assert_eq!(request.path, "/v1/log/agent-event");
        assert!(request.body.contains("\"eventId\":\"evt-1\""));
    }

    #[tokio::test]
    async fn falls_back_to_http_when_redis_ping_times_out() {
        let redis = RedisFixture::start(RedisMode::Stall);
        let http = HttpFixture::start(200);
        let transport = AgentEventLogTransport::new(
            Some(Arc::new(AppRedis::new(redis.url()))),
            "agent-events".to_string(),
            true,
            http.url(),
            Duration::from_millis(50),
            "authorization".to_string(),
            "agent-token".to_string(),
            reqwest::Client::new(),
        );

        transport.send(&payload_fixture()).await.unwrap();

        let request = http.receive();
        assert_eq!(request.path, "/v1/log/agent-event");
        assert!(request.body.contains("\"eventId\":\"evt-1\""));
    }

    #[tokio::test]
    async fn falls_back_to_http_when_redis_xadd_times_out() {
        let redis = RedisFixture::start(RedisMode::XaddStall);
        let http = HttpFixture::start(200);
        let transport = AgentEventLogTransport::new(
            Some(Arc::new(AppRedis::new(redis.url()))),
            "agent-events".to_string(),
            true,
            http.url(),
            Duration::from_millis(50),
            "authorization".to_string(),
            "agent-token".to_string(),
            reqwest::Client::new(),
        );

        transport.send(&payload_fixture()).await.unwrap();

        let request = http.receive();
        assert_eq!(request.path, "/v1/log/agent-event");
        assert!(request.body.contains("\"eventId\":\"evt-1\""));
    }

    #[tokio::test]
    async fn sends_custom_auth_header_as_raw_token() {
        let fixture = HttpFixture::start(200);
        let transport = AgentEventLogTransport::new(
            None,
            "agent-events".to_string(),
            true,
            fixture.url(),
            Duration::from_secs(1),
            "x-alephant-internal-token".to_string(),
            "agent-token".to_string(),
            reqwest::Client::new(),
        );

        transport.send(&payload_fixture()).await.unwrap();

        let request = fixture.receive();
        assert_eq!(
            request.header("x-alephant-internal-token"),
            Some("agent-token")
        );
    }

    fn payload_fixture() -> AgentEventLogPayload {
        serde_json::from_value(json!({
            "version": "2026-05-27",
            "eventId": "evt-1",
            "workspaceId": "workspace-1",
            "eventType": "tool.call.completed",
            "eventSource": "langgraph",
            "observedAt": "2026-05-30T12:00:00Z",
            "stepSource": "runtime",
            "stepConfidence": "high",
            "agentTrustLevel": "auth_bound",
            "contextConflict": false,
            "stepIdConflict": false,
            "metadata": "{}"
        }))
        .unwrap()
    }

    struct HttpFixture {
        url: Url,
        rx: mpsc::Receiver<CapturedRequest>,
        handle: thread::JoinHandle<()>,
    }

    impl HttpFixture {
        fn start(status_code: u16) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = Url::parse(&format!(
                "http://{}",
                listener.local_addr().unwrap()
            ))
            .unwrap();
            let (tx, rx) = mpsc::channel();
            let handle = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_request(&mut stream);
                tx.send(request).unwrap();
                let reason = if status_code == 200 {
                    "OK"
                } else {
                    "Internal Server Error"
                };
                write!(
                    stream,
                    "HTTP/1.1 {status_code} {reason}\r\nContent-Length: \
                     0\r\n\r\n"
                )
                .unwrap();
            });

            Self { url, rx, handle }
        }

        fn url(&self) -> Url {
            self.url.join("/v1/log/agent-event").unwrap()
        }

        fn receive(self) -> CapturedRequest {
            let request = self.rx.recv_timeout(Duration::from_secs(2)).unwrap();
            self.handle.join().unwrap();
            request
        }
    }

    struct CapturedRequest {
        method: String,
        path: String,
        headers: Vec<(String, String)>,
        body: String,
    }

    impl CapturedRequest {
        fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(header_name, _)| header_name.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.as_str())
        }
    }

    fn read_request(stream: &mut TcpStream) -> CapturedRequest {
        let mut bytes = Vec::new();
        let mut buf = [0; 1024];
        loop {
            let n = stream.read(&mut buf).unwrap();
            bytes.extend_from_slice(&buf[..n]);
            if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }

        let header_end = bytes
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .unwrap()
            + 4;
        let header_text =
            String::from_utf8(bytes[..header_end].to_vec()).unwrap();
        let mut lines = header_text.split("\r\n");
        let request_line = lines.next().unwrap();
        let mut request_parts = request_line.split_whitespace();
        let method = request_parts.next().unwrap().to_string();
        let path = request_parts.next().unwrap().to_string();
        let headers = lines
            .filter(|line| !line.is_empty())
            .map(|line| {
                let (name, value) = line.split_once(':').unwrap();
                (name.to_string(), value.trim().to_string())
            })
            .collect::<Vec<_>>();
        let content_length = headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, value)| value.parse::<usize>().ok())
            .unwrap_or(0);

        let mut body = bytes[header_end..].to_vec();
        while body.len() < content_length {
            let n = stream.read(&mut buf).unwrap();
            body.extend_from_slice(&buf[..n]);
        }

        CapturedRequest {
            method,
            path,
            headers,
            body: String::from_utf8(body[..content_length].to_vec()).unwrap(),
        }
    }

    enum RedisMode {
        XaddError,
        XaddStall,
        Stall,
    }

    struct RedisFixture {
        url: Url,
        handle: thread::JoinHandle<()>,
    }

    impl RedisFixture {
        fn start(mode: RedisMode) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = Url::parse(&format!(
                "redis://{}",
                listener.local_addr().unwrap()
            ))
            .unwrap();
            let handle = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                if matches!(mode, RedisMode::Stall) {
                    thread::sleep(Duration::from_millis(250));
                    return;
                }
                handle_redis_connection(&mut stream, mode);
            });
            Self { url, handle }
        }

        fn url(&self) -> Url {
            self.url.clone()
        }
    }

    impl Drop for RedisFixture {
        fn drop(&mut self) {
            let _ = self.handle.thread().id();
        }
    }

    fn handle_redis_connection(stream: &mut TcpStream, mode: RedisMode) {
        let mut buffer = Vec::new();
        let mut chunk = [0_u8; 1024];
        loop {
            let Ok(read) = stream.read(&mut chunk) else {
                return;
            };
            if read == 0 {
                return;
            }
            buffer.extend_from_slice(&chunk[..read]);

            while let Some((command, consumed)) = parse_resp_command(&buffer) {
                buffer.drain(..consumed);
                let response = redis_response(&command, &mode);
                if stream.write_all(response.as_bytes()).is_err() {
                    return;
                }
            }
        }
    }

    fn redis_response(command: &[String], mode: &RedisMode) -> String {
        match command
            .first()
            .map(|command| command.to_ascii_uppercase())
            .as_deref()
        {
            Some("PING") => "+PONG\r\n".to_string(),
            Some("XADD") if matches!(mode, RedisMode::XaddStall) => {
                thread::sleep(Duration::from_millis(250));
                "+OK\r\n".to_string()
            }
            Some("XADD") if matches!(mode, RedisMode::XaddError) => {
                "-ERR xadd failed\r\n".to_string()
            }
            Some("XADD") => "$3\r\n0-1\r\n".to_string(),
            Some("CLIENT" | "HELLO" | "SET" | "EXPIRE") => {
                "+OK\r\n".to_string()
            }
            _ => "+OK\r\n".to_string(),
        }
    }

    fn parse_resp_command(buffer: &[u8]) -> Option<(Vec<String>, usize)> {
        if buffer.first().copied()? != b'*' {
            return None;
        }
        let (count_line, mut index) = read_line(buffer, 1)?;
        let count = std::str::from_utf8(count_line)
            .ok()?
            .parse::<usize>()
            .ok()?;
        let mut parts = Vec::with_capacity(count);

        for _ in 0..count {
            if buffer.get(index).copied()? != b'$' {
                return None;
            }
            let (len_line, next_index) = read_line(buffer, index + 1)?;
            index = next_index;
            let len =
                std::str::from_utf8(len_line).ok()?.parse::<usize>().ok()?;
            if buffer.len() < index + len + 2 {
                return None;
            }
            let part = std::str::from_utf8(&buffer[index..index + len])
                .ok()?
                .to_string();
            parts.push(part);
            index += len;
            if buffer.get(index..index + 2)? != b"\r\n" {
                return None;
            }
            index += 2;
        }

        Some((parts, index))
    }

    fn read_line(buffer: &[u8], start: usize) -> Option<(&[u8], usize)> {
        let end = buffer[start..]
            .windows(2)
            .position(|window| window == b"\r\n")?
            + start;
        Some((&buffer[start..end], end + 2))
    }
}
