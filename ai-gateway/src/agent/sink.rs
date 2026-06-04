use std::time::Duration;

use url::Url;

use crate::{
    agent::{
        event::AgentEventEnvelope,
        log_payload::AgentEventLogPayload,
        log_transport::{AgentEventLogTransport, AgentEventLogTransportError},
    },
    app_state::AppState,
    types::extensions::AuthContext,
};

pub async fn emit_agent_event(
    app_state: &AppState,
    auth_ctx: &AuthContext,
    event: &AgentEventEnvelope,
) -> Result<(), AgentSinkError> {
    let config = app_state.config();
    let cfg = &config.agent;
    let endpoint = resolve_event_log_endpoint(
        &config.alephant.log_collector_url,
        &cfg.event_log_http_endpoint,
    )?;
    let transport = AgentEventLogTransport::new(
        app_state.redis().cloned(),
        cfg.event_stream_key.clone(),
        cfg.event_log_http_fallback_enabled,
        endpoint,
        Duration::from_millis(cfg.event_log_http_timeout_ms),
        auth_ctx.api_key.expose().to_string(),
        reqwest::Client::new(),
    );
    let payload = AgentEventLogPayload::from_envelope_with_auth(event, auth_ctx);
    transport.send(&payload).await?;
    Ok(())
}

fn resolve_event_log_endpoint(
    log_collector_url: &Url,
    endpoint: &str,
) -> Result<Url, url::ParseError> {
    if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
        endpoint.parse()
    } else {
        log_collector_url.join(endpoint)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AgentSinkError {
    #[error("agent event log transport failed: {0}")]
    Transport(#[from] AgentEventLogTransportError),
    #[error("agent event log endpoint URL is invalid: {0}")]
    Url(#[from] url::ParseError),
}
