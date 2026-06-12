use std::{
    collections::VecDeque,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::Duration,
};

use http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};

use crate::{
    agent::tools::{
        executor::ToolExecutionContext, mcp_sse::target_hash::canonical_mcp_sse_target_hash,
        types::ToolCallRequest,
    },
    config::agent::{
        AgentToolEgressPolicyConfig, AgentToolTargetConfig, AgentToolTargetKind, AgentToolsConfig,
    },
};

#[derive(Debug)]
pub enum McpSseServerEvent {
    Data(serde_json::Value),
    ResponseForRequest {
        result: serde_json::Value,
    },
    HeldResponseForRequest {
        receiver: Arc<Mutex<mpsc::Receiver<serde_json::Value>>>,
    },
    Raw(String),
}

impl McpSseServerEvent {
    fn to_sse_text(&self) -> String {
        match self {
            Self::Data(value) => format!("data: {value}\n\n"),
            Self::ResponseForRequest { result } => {
                format!("data: {result}\n\n")
            }
            Self::HeldResponseForRequest { .. } => String::new(),
            Self::Raw(value) => value.clone(),
        }
    }

    fn bind_request_id(self, request: &serde_json::Value) -> Self {
        let result = match self {
            Self::ResponseForRequest { result } => result,
            Self::HeldResponseForRequest { receiver } => receiver
                .lock()
                .expect("held MCP SSE response")
                .recv()
                .unwrap_or_else(|_| {
                    serde_json::json!({
                        "content": [],
                        "isError": true,
                    })
                }),
            _ => return self,
        };
        let id = request
            .get("id")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        Self::Data(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        }))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedMcpSseRequest {
    pub method: String,
    pub path: String,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl RecordedMcpSseRequest {
    pub fn body_json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).expect("captured body is json")
    }
}

pub struct McpSseFixture {
    base_url: String,
    sse_url: String,
    message_url: String,
    requests: Arc<Mutex<Vec<RecordedMcpSseRequest>>>,
    held_response: Option<mpsc::Sender<serde_json::Value>>,
    shutdown: Option<mpsc::Sender<()>>,
    handle: Option<thread::JoinHandle<()>>,
}

impl McpSseFixture {
    pub fn start(events: Vec<McpSseServerEvent>) -> Option<Self> {
        Self::start_with_endpoint("/message", events)
    }

    pub fn start_without_call_response() -> Option<Self> {
        Self::start(vec![sse_json_rpc_response(
            "init-exec-1",
            serde_json::json!({
                "protocolVersion": crate::agent::tools::mcp_streamable_http::json_rpc::CLIENT_PROTOCOL_VERSION,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "fixture", "version": "1"}
            }),
        )])
    }

    pub fn start_with_large_event(size: usize) -> Option<Self> {
        Self::start(vec![sse_raw_data(serde_json::json!({
            "jsonrpc": "2.0",
            "id": "init-exec-1",
            "result": {
                "protocolVersion": crate::agent::tools::mcp_streamable_http::json_rpc::CLIENT_PROTOCOL_VERSION,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "fixture", "version": "1"},
                "padding": "x".repeat(size),
            }
        }))])
    }

    pub fn start_holding_call_response() -> Option<Self> {
        let (release_tx, release_rx) = mpsc::channel();
        let held_response = Arc::new(Mutex::new(release_rx));
        Self::start_with_endpoint_and_release(
            "/message",
            vec![
                sse_json_rpc_response_for_request(serde_json::json!({
                    "protocolVersion": crate::agent::tools::mcp_streamable_http::json_rpc::CLIENT_PROTOCOL_VERSION,
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "fixture", "version": "1"}
                })),
                McpSseServerEvent::HeldResponseForRequest {
                    receiver: held_response,
                },
            ],
            Some(release_tx),
        )
    }

    pub fn start_with_endpoint(endpoint: &str, events: Vec<McpSseServerEvent>) -> Option<Self> {
        Self::start_with_endpoint_and_release(endpoint, events, None)
    }

    fn start_with_endpoint_and_release(
        endpoint: &str,
        events: Vec<McpSseServerEvent>,
        held_response: Option<mpsc::Sender<serde_json::Value>>,
    ) -> Option<Self> {
        let listener = match TcpListener::bind("127.0.0.1:0") {
            Ok(listener) => listener,
            Err(err) => {
                eprintln!("skipping MCP SSE fixture: {err}");
                return None;
            }
        };
        listener
            .set_nonblocking(true)
            .expect("fixture listener nonblocking");
        let addr = listener.local_addr().expect("fixture addr");
        let base_url = format!("http://{addr}");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (shutdown_tx, shutdown_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let handle_requests = requests.clone();
        let endpoint = endpoint.to_string();
        let handle = thread::spawn(move || {
            serve_fixture(
                listener,
                endpoint,
                events,
                handle_requests,
                shutdown_rx,
                ready_tx,
            );
        });
        let _ = ready_rx.recv_timeout(Duration::from_secs(1));

        Some(Self {
            base_url: base_url.clone(),
            sse_url: format!("{base_url}/sse"),
            message_url: format!("{base_url}/message"),
            requests,
            held_response,
            shutdown: Some(shutdown_tx),
            handle: Some(handle),
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn sse_url(&self) -> &str {
        &self.sse_url
    }

    pub fn message_url(&self) -> &str {
        &self.message_url
    }

    pub fn requests(&self) -> Vec<RecordedMcpSseRequest> {
        self.requests.lock().expect("fixture requests").clone()
    }

    pub fn release_held_call(&self, text: &str) {
        if let Some(sender) = &self.held_response {
            let _ = sender.send(serde_json::json!({
                "content": [{"type": "text", "text": text}],
                "isError": false,
            }));
        }
    }
}

impl Drop for McpSseFixture {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

pub fn sse_json_rpc_response(id: &str, result: serde_json::Value) -> McpSseServerEvent {
    McpSseServerEvent::Data(serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    }))
}

pub fn sse_json_rpc_error(id: &str, code: i64, message: &str) -> McpSseServerEvent {
    McpSseServerEvent::Data(serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": code, "message": message},
    }))
}

pub fn sse_json_rpc_response_for_request(result: serde_json::Value) -> McpSseServerEvent {
    McpSseServerEvent::ResponseForRequest { result }
}

pub fn sse_raw_data(value: serde_json::Value) -> McpSseServerEvent {
    McpSseServerEvent::Data(value)
}

pub fn test_mcp_sse_target(url: &str) -> AgentToolTargetConfig {
    AgentToolTargetConfig {
        tool_id: "docs.search".to_string(),
        name: "Docs Search".to_string(),
        description: "Search docs through a traditional MCP SSE target".to_string(),
        kind: AgentToolTargetKind::McpSse,
        url: Some(url.to_string()),
        method: "GET".to_string(),
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
    ToolExecutionContext {
        workspace_id: "workspace-test".to_string(),
        virtual_key_id: Some("vk-test".to_string()),
        agent_id: "agent-test".to_string(),
        caller_principal_id: "caller-test".to_string(),
        target_id: target.tool_id.clone(),
        target_revision: 0,
        target_hash: canonical_mcp_sse_target_hash(target, &tools_cfg),
        auth_revision: "0/static".to_string(),
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

fn serve_fixture(
    listener: TcpListener,
    endpoint: String,
    events: Vec<McpSseServerEvent>,
    requests: Arc<Mutex<Vec<RecordedMcpSseRequest>>>,
    shutdown: mpsc::Receiver<()>,
    ready: mpsc::Sender<()>,
) {
    let events = Arc::new(Mutex::new(VecDeque::from(events)));
    let (event_tx, event_rx) = mpsc::channel::<McpSseServerEvent>();
    let event_rx = Arc::new(Mutex::new(Some(event_rx)));
    let _ = ready.send(());

    loop {
        if shutdown.try_recv().is_ok() {
            return;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                let thread_requests = requests.clone();
                let thread_events = events.clone();
                let thread_event_tx = event_tx.clone();
                let thread_event_rx = event_rx.clone();
                let thread_endpoint = endpoint.clone();
                thread::spawn(move || {
                    handle_connection(
                        stream,
                        thread_endpoint,
                        thread_requests,
                        thread_events,
                        thread_event_tx,
                        thread_event_rx,
                    );
                });
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(5));
            }
            Err(_) => return,
        }
    }
}

fn handle_connection(
    mut stream: TcpStream,
    endpoint: String,
    requests: Arc<Mutex<Vec<RecordedMcpSseRequest>>>,
    events: Arc<Mutex<VecDeque<McpSseServerEvent>>>,
    event_tx: mpsc::Sender<McpSseServerEvent>,
    event_rx: Arc<Mutex<Option<mpsc::Receiver<McpSseServerEvent>>>>,
) {
    let Some(request) = read_request(&mut stream) else {
        return;
    };

    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/sse") => {
            let Some(rx) = event_rx.lock().expect("event rx").take() else {
                let _ = write_status(&mut stream, StatusCode::CONFLICT);
                return;
            };
            let _ = serve_sse_stream(stream, &endpoint, rx);
        }
        ("POST", "/message") => {
            let body_json = request.body_json();
            let should_emit_response = body_json.get("id").is_some_and(|id| !id.is_null());
            requests.lock().expect("fixture requests").push(request);
            if should_emit_response
                && let Some(event) = events.lock().expect("fixture events").pop_front()
            {
                let _ = event_tx.send(event.bind_request_id(&body_json));
            }
            let _ = write_status(&mut stream, StatusCode::ACCEPTED);
        }
        _ => {
            let _ = write_status(&mut stream, StatusCode::NOT_FOUND);
        }
    }
}

fn serve_sse_stream(
    mut stream: TcpStream,
    endpoint: &str,
    events: mpsc::Receiver<McpSseServerEvent>,
) -> std::io::Result<()> {
    write!(stream, "HTTP/1.1 200 OK\r\n")?;
    write!(stream, "content-type: text/event-stream\r\n")?;
    write!(stream, "cache-control: no-cache\r\n")?;
    write!(stream, "connection: keep-alive\r\n")?;
    write!(stream, "\r\n")?;
    write!(stream, "event: endpoint\ndata: {endpoint}\n\n")?;
    stream.flush()?;

    while let Ok(event) = events.recv() {
        stream.write_all(event.to_sse_text().as_bytes())?;
        stream.flush()?;
    }
    Ok(())
}

fn write_status(stream: &mut TcpStream, status: StatusCode) -> std::io::Result<()> {
    let reason = status.canonical_reason().unwrap_or("status");
    write!(stream, "HTTP/1.1 {} {reason}\r\n", status.as_u16())?;
    write!(stream, "content-length: 0\r\n")?;
    write!(stream, "connection: close\r\n")?;
    write!(stream, "\r\n")?;
    stream.flush()
}

fn read_request(stream: &mut TcpStream) -> Option<RecordedMcpSseRequest> {
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

    Some(RecordedMcpSseRequest {
        method,
        path,
        headers,
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mcp_sse_fixture_serves_endpoint_and_records_messages() {
        let Some(fixture) = McpSseFixture::start(vec![sse_json_rpc_response(
            "init-1",
            serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "fixture", "version": "1"}
            }),
        )]) else {
            return;
        };

        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("client");
        let sse = client
            .get(fixture.sse_url())
            .send()
            .await
            .expect("sse response");
        assert_eq!(sse.status(), reqwest::StatusCode::OK);

        let response = client
            .post(fixture.message_url())
            .json(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": "init-1",
                "method": "initialize",
                "params": {}
            }))
            .send()
            .await
            .expect("message response");
        assert_eq!(response.status(), reqwest::StatusCode::ACCEPTED);

        let requests = fixture.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].body_json()["method"], "initialize");
    }

    #[test]
    fn sse_helpers_build_json_rpc_events() {
        let event = sse_json_rpc_error("call-1", -32603, "tool failed").to_sse_text();

        assert!(event.contains("\"id\":\"call-1\""));
        assert!(event.contains("\"code\":-32603"));
    }
}
