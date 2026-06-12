use std::{
    collections::VecDeque,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::Duration,
};

use chrono::Utc;
use http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};

use crate::{
    agent::tools::{
        executor::ToolExecutionContext,
        mcp_streamable_http::{session::McpStreamableSession, target_hash::canonical_target_hash},
        types::ToolCallRequest,
    },
    config::agent::{
        AgentToolEgressPolicyConfig, AgentToolTargetConfig, AgentToolTargetKind, AgentToolsConfig,
    },
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedRequest {
    pub method: String,
    pub path: String,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl CapturedRequest {
    pub fn body_json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).expect("captured body is json")
    }
}

#[derive(Debug, Clone)]
pub struct FixtureResponse {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
    include_content_length: bool,
    hold_open_after_body: Option<Duration>,
}

impl FixtureResponse {
    pub fn new(status: StatusCode, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            headers: HeaderMap::new(),
            body: body.into(),
            include_content_length: true,
            hold_open_after_body: None,
        }
    }

    pub fn header(mut self, name: HeaderName, value: HeaderValue) -> Self {
        self.headers.insert(name, value);
        self
    }

    pub fn hold_open_after_body(mut self, duration: Duration) -> Self {
        self.include_content_length = false;
        self.hold_open_after_body = Some(duration);
        self
    }

    fn write_to(&self, stream: &mut TcpStream) -> std::io::Result<()> {
        write!(stream, "HTTP/1.1 {}\r\n", self.status.as_u16())?;
        write!(stream, "connection: close\r\n")?;
        if self.include_content_length {
            write!(stream, "content-length: {}\r\n", self.body.len())?;
        }
        for (name, value) in &self.headers {
            write!(
                stream,
                "{}: {}\r\n",
                name.as_str(),
                value.to_str().unwrap_or_default()
            )?;
        }
        stream.write_all(b"\r\n")?;
        stream.write_all(&self.body)?;
        stream.flush()?;
        if let Some(duration) = self.hold_open_after_body {
            thread::sleep(duration);
        }
        Ok(())
    }
}

pub struct StreamableFixture {
    url: String,
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
    shutdown: Option<mpsc::Sender<()>>,
    handle: Option<thread::JoinHandle<()>>,
}

impl StreamableFixture {
    pub fn start(responses: Vec<FixtureResponse>) -> Option<Self> {
        let listener = match TcpListener::bind("127.0.0.1:0") {
            Ok(listener) => listener,
            Err(err) => {
                eprintln!("skipping MCP streamable HTTP fixture: {err}");
                return None;
            }
        };
        listener
            .set_nonblocking(true)
            .expect("fixture listener nonblocking");
        let addr = listener.local_addr().expect("fixture addr");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let thread_requests = requests.clone();
        let (shutdown_tx, shutdown_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            serve_fixture(listener, responses, thread_requests, shutdown_rx, ready_tx);
        });
        let _ = ready_rx.recv_timeout(Duration::from_secs(1));

        Some(Self {
            url: format!("http://{addr}/mcp"),
            requests,
            shutdown: Some(shutdown_tx),
            handle: Some(handle),
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn requests(&self) -> Vec<CapturedRequest> {
        self.requests.lock().expect("fixture requests").clone()
    }
}

impl Drop for StreamableFixture {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

pub fn response_json(value: serde_json::Value) -> FixtureResponse {
    FixtureResponse::new(StatusCode::OK, value.to_string()).header(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    )
}

pub fn initialize_response(session_id: &str) -> FixtureResponse {
    initialize_response_for_execution("exec-1", session_id)
}

pub fn initialize_response_for_execution(execution_id: &str, session_id: &str) -> FixtureResponse {
    response_json(serde_json::json!({
        "jsonrpc": "2.0",
        "id": format!("init_{execution_id}"),
        "result": {
            "protocolVersion": "2025-06-18",
            "capabilities": {
                "tools": {}
            },
            "serverInfo": {
                "name": "streamable-fixture",
                "version": "test"
            }
        }
    }))
    .header(
        HeaderName::from_static("mcp-session-id"),
        HeaderValue::from_str(session_id).expect("valid session header"),
    )
}

pub fn tool_result_response(result: serde_json::Value) -> FixtureResponse {
    tool_result_response_for_execution("exec-1", result)
}

pub fn tool_result_response_for_execution(
    execution_id: &str,
    result: serde_json::Value,
) -> FixtureResponse {
    response_json(serde_json::json!({
        "jsonrpc": "2.0",
        "id": execution_id,
        "result": result
    }))
}

pub fn json_rpc_error_response(code: i64, message: &str) -> FixtureResponse {
    json_rpc_error_response_for_execution("exec-1", code, message)
}

pub fn json_rpc_error_response_for_execution(
    execution_id: &str,
    code: i64,
    message: &str,
) -> FixtureResponse {
    response_json(serde_json::json!({
        "jsonrpc": "2.0",
        "id": execution_id,
        "error": {
            "code": code,
            "message": message
        }
    }))
}

pub fn sse_response(events: &[&str]) -> FixtureResponse {
    let body = events
        .iter()
        .map(|event| format!("data: {event}\n\n"))
        .collect::<String>();
    sse_raw_response(body)
}

pub fn sse_raw_response(body: impl Into<Vec<u8>>) -> FixtureResponse {
    FixtureResponse::new(StatusCode::OK, body).header(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    )
}

pub fn status_response(status: StatusCode) -> FixtureResponse {
    FixtureResponse::new(status, Vec::new())
}

pub fn test_streamable_target(url: &str) -> AgentToolTargetConfig {
    AgentToolTargetConfig {
        tool_id: "docs.search".to_string(),
        name: "Docs Search".to_string(),
        description: "Search docs through a streamable MCP target".to_string(),
        kind: AgentToolTargetKind::McpStreamableHttp,
        url: Some(url.to_string()),
        method: "POST".to_string(),
        ..AgentToolTargetConfig::default()
    }
}

pub fn test_request() -> ToolCallRequest {
    ToolCallRequest {
        tool_call_id: Some("call-1".to_string()),
        tool_execution_id: Some("exec-1".to_string()),
        tool_id: "docs.search".to_string(),
        arguments: serde_json::json!({ "query": "mcp" }),
        ..ToolCallRequest::default()
    }
}

pub fn test_egress_policy() -> AgentToolEgressPolicyConfig {
    AgentToolEgressPolicyConfig {
        https_only: false,
        block_loopback: false,
        block_link_local: false,
        block_metadata_ip: true,
        block_private_network: false,
        allow_environment_proxy: false,
    }
}

pub fn test_context(target: &AgentToolTargetConfig) -> ToolExecutionContext {
    let tools_cfg = AgentToolsConfig::default();
    let auth_revision = "auth-test".to_string();
    ToolExecutionContext {
        workspace_id: "workspace-test".to_string(),
        virtual_key_id: Some("vk-test".to_string()),
        agent_id: "agent-test".to_string(),
        caller_principal_id: "caller-test".to_string(),
        target_id: target.tool_id.clone(),
        target_revision: 1,
        target_hash: canonical_target_hash(target, 1, &auth_revision, &tools_cfg),
        auth_revision,
        redis: None,
        mcp_session_cache_ttl_secs: tools_cfg.mcp_session_cache_ttl_secs,
        mcp_session_lock_ttl_secs: tools_cfg.mcp_session_lock_ttl_secs,
        mcp_session_max_concurrent_per_session: tools_cfg.mcp_session_max_concurrent_per_session,
        mcp_sse_max_event_bytes: tools_cfg.mcp_sse_max_event_bytes,
        mcp_sse_max_line_bytes: tools_cfg.mcp_sse_max_line_bytes,
        mcp_sse_max_events: tools_cfg.mcp_sse_max_events,
        mcp_sse_max_batch_items: tools_cfg.mcp_sse_max_batch_items,
        mcp_sse_idle_timeout_ms: tools_cfg.mcp_sse_idle_timeout_ms,
    }
}

pub fn test_session_for_ctx(ctx: &ToolExecutionContext, session_id: &str) -> McpStreamableSession {
    let now = Utc::now();
    McpStreamableSession {
        session_id: session_id.to_string(),
        negotiated_protocol_version: "2025-06-18".to_string(),
        target_hash: ctx.target_hash.clone(),
        auth_revision: ctx.auth_revision.clone(),
        server_info: serde_json::json!({
            "name": "streamable-fixture",
            "version": "test"
        }),
        capabilities: serde_json::json!({"tools": {}}),
        created_at: now,
        last_used_at: now,
        expires_at: now + chrono::Duration::seconds(60),
    }
}

fn serve_fixture(
    listener: TcpListener,
    responses: Vec<FixtureResponse>,
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
    shutdown: mpsc::Receiver<()>,
    ready: mpsc::Sender<()>,
) {
    let responses = Arc::new(Mutex::new(VecDeque::from(responses)));
    let _ = ready.send(());
    loop {
        if shutdown.try_recv().is_ok() {
            return;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                let thread_requests = requests.clone();
                let thread_responses = responses.clone();
                thread::spawn(move || {
                    handle_fixture_connection(stream, thread_responses, thread_requests);
                });
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(5));
            }
            Err(_) => return,
        }
    }
}

fn handle_fixture_connection(
    mut stream: TcpStream,
    responses: Arc<Mutex<VecDeque<FixtureResponse>>>,
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
) {
    let Some(request) = read_request(&mut stream) else {
        return;
    };
    requests.lock().expect("fixture requests").push(request);
    let response = responses
        .lock()
        .expect("fixture responses")
        .pop_front()
        .unwrap_or_else(|| status_response(StatusCode::NO_CONTENT));
    let _ = response.write_to(&mut stream);
}

fn read_request(stream: &mut TcpStream) -> Option<CapturedRequest> {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 1024];
    loop {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..n]);
        if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }

    let headers_end = buffer.windows(4).position(|window| window == b"\r\n\r\n")? + 4;
    let header_text = String::from_utf8_lossy(&buffer[..headers_end]);
    let mut lines = header_text.lines();
    let request_line = lines.next()?;
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts.next()?.to_string();
    let path = request_parts.next()?.to_string();
    let mut headers = HeaderMap::new();
    let mut content_length = 0_usize;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = HeaderName::from_bytes(name.trim().as_bytes()).ok()?;
        let value = HeaderValue::from_str(value.trim()).ok()?;
        if name == header::CONTENT_LENGTH {
            content_length = value.to_str().ok()?.parse().ok()?;
        }
        headers.insert(name, value);
    }

    let mut body = buffer[headers_end..].to_vec();
    while body.len() < content_length {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(content_length);

    Some(CapturedRequest {
        method,
        path,
        headers,
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builders_create_streamable_target_request_policy_and_context() {
        let target = test_streamable_target("http://127.0.0.1:1234/mcp");
        let request = test_request();
        let policy = test_egress_policy();
        let ctx = test_context(&target);
        let session = test_session_for_ctx(&ctx, "session-1");

        assert_eq!(target.kind, AgentToolTargetKind::McpStreamableHttp);
        assert_eq!(request.tool_id, target.tool_id);
        assert!(!policy.https_only);
        assert!(!policy.block_loopback);
        assert_eq!(ctx.target_id, target.tool_id);
        assert!(ctx.target_hash.starts_with("sha256:"));
        assert_eq!(session.session_id, "session-1");
        assert_eq!(session.target_hash, ctx.target_hash);
        assert_eq!(session.auth_revision, ctx.auth_revision);
        assert_eq!(session.negotiated_protocol_version, "2025-06-18");
    }

    #[test]
    fn response_builders_set_protocol_headers() {
        let response = initialize_response("session-1");

        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(
            response.headers["mcp-session-id"],
            HeaderValue::from_static("session-1")
        );
        assert_eq!(
            response.headers[header::CONTENT_TYPE],
            HeaderValue::from_static("application/json")
        );
    }

    #[test]
    fn fixture_start_can_record_requests_when_loopback_bind_is_available() {
        let Some(fixture) = StreamableFixture::start(vec![tool_result_response(
            serde_json::json!({ "content": [] }),
        )]) else {
            return;
        };

        let mut stream = TcpStream::connect(
            fixture
                .url()
                .trim_start_matches("http://")
                .trim_end_matches("/mcp"),
        )
        .expect("connect fixture");
        stream
            .write_all(b"POST /mcp HTTP/1.1\r\nhost: 127.0.0.1\r\ncontent-length: 2\r\n\r\n{}")
            .expect("write fixture request");
        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .expect("read fixture response");

        let requests = fixture.requests();
        assert!(response.starts_with("HTTP/1.1 200"));
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "POST");
        assert_eq!(requests[0].path, "/mcp");
        assert_eq!(requests[0].body_json(), serde_json::json!({}));
    }

    #[test]
    fn fixture_drop_does_not_block_on_stalled_client() {
        let Some(fixture) = StreamableFixture::start(vec![status_response(StatusCode::OK)]) else {
            return;
        };

        let _stream = TcpStream::connect(
            fixture
                .url()
                .trim_start_matches("http://")
                .trim_end_matches("/mcp"),
        )
        .expect("connect fixture");
        let (done_tx, done_rx) = mpsc::channel();
        thread::spawn(move || {
            drop(fixture);
            let _ = done_tx.send(());
        });

        done_rx
            .recv_timeout(Duration::from_millis(500))
            .expect("fixture drop should not block on stalled client");
    }
}
