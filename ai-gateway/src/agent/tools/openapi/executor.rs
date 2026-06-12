use std::time::{Duration, Instant};

use futures::TryStreamExt;
use reqwest::header::CONTENT_TYPE;
use url::Url;

use crate::{
    agent::tools::{
        executor::ToolExecutionErrorKind,
        openapi::{
            egress::validate_openapi_egress,
            mapping::{OpenApiRequestPlan, build_request_plan},
            outcome::{OpenApiOutcomeInput, OpenApiOutcomeStatus, decide},
            types::{
                OpenApiBodyMapping, OpenApiParameterLocation, OpenApiParameterMapping,
                OpenApiValueSource, RuntimeOpenApiTarget,
            },
        },
        types::{
            ToolCallRequest, ToolCallResponse, ToolCost, ToolExecutionEvents, ToolGatewayMetadata,
            ToolPolicySummary,
        },
    },
    config::agent::{AgentToolEgressPolicyConfig, AgentToolTargetConfig},
};

pub async fn execute_openapi_tool(
    target: &AgentToolTargetConfig,
    request: &ToolCallRequest,
    egress_policy: &AgentToolEgressPolicyConfig,
    default_timeout_ms: u64,
    max_request_bytes: usize,
    max_response_bytes: usize,
) -> Result<ToolCallResponse, ToolExecutionErrorKind> {
    let Some(runtime_target) = runtime_target_from_static(target, request)? else {
        return Err(ToolExecutionErrorKind::ToolTargetUnavailable);
    };

    execute_runtime_openapi_tool(
        &runtime_target,
        &target.method,
        request,
        egress_policy,
        OpenApiExecutionOptions {
            target_id: target.tool_id.clone(),
            fixed_micros: target.rate_card.fixed_micros,
            currency: target.rate_card.currency.clone(),
            charge_on_failure: false,
            timeout_ms: effective_timeout_ms(
                target.timeout_ms,
                request.timeout_ms,
                default_timeout_ms,
            ),
            max_request_bytes,
            max_response_bytes,
        },
    )
    .await
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OpenApiExecutionOptions {
    target_id: String,
    fixed_micros: u64,
    currency: String,
    charge_on_failure: bool,
    timeout_ms: u64,
    max_request_bytes: usize,
    max_response_bytes: usize,
}

async fn execute_runtime_openapi_tool(
    target: &RuntimeOpenApiTarget,
    method: &str,
    request: &ToolCallRequest,
    egress_policy: &AgentToolEgressPolicyConfig,
    options: OpenApiExecutionOptions,
) -> Result<ToolCallResponse, ToolExecutionErrorKind> {
    let tool_execution_id = request
        .tool_execution_id
        .clone()
        .unwrap_or_else(|| format!("exec_{}", uuid::Uuid::new_v4().simple()));

    let started = Instant::now();
    let plan = match build_request_plan(method, target, &request.arguments) {
        Ok(plan) => plan,
        Err(_) => {
            return Ok(openapi_response(
                target,
                request,
                &options,
                &tool_execution_id,
                OpenApiOutcomeStatus::SchemaInvalid,
                None,
                None,
                None,
                started,
            ));
        }
    };
    if validate_openapi_egress(target, &plan, egress_policy).is_err() {
        return Ok(openapi_response(
            target,
            request,
            &options,
            &tool_execution_id,
            OpenApiOutcomeStatus::EgressBlocked,
            Some(&plan),
            None,
            None,
            started,
        ));
    }
    if plan.request_bytes > options.max_request_bytes as u64 {
        return Ok(openapi_response(
            target,
            request,
            &options,
            &tool_execution_id,
            OpenApiOutcomeStatus::RequestTooLarge,
            Some(&plan),
            None,
            None,
            started,
        ));
    }
    if reqwest::Method::from_bytes(plan.method.as_bytes()).is_err() {
        return Ok(openapi_response(
            target,
            request,
            &options,
            &tool_execution_id,
            OpenApiOutcomeStatus::SchemaInvalid,
            Some(&plan),
            None,
            None,
            started,
        ));
    }

    let client = openapi_client(options.timeout_ms, egress_policy)
        .map_err(|_| ToolExecutionErrorKind::ToolTargetUnavailable)?;
    let dispatched = dispatch_request(&client, &plan).await;
    let response = match dispatched {
        Ok(response) => response,
        Err(error) if error.is_timeout() => {
            return Ok(openapi_response(
                target,
                request,
                &options,
                &tool_execution_id,
                OpenApiOutcomeStatus::Timeout,
                Some(&plan),
                None,
                None,
                started,
            ));
        }
        Err(_) => {
            return Ok(openapi_response(
                target,
                request,
                &options,
                &tool_execution_id,
                OpenApiOutcomeStatus::InternalError,
                Some(&plan),
                None,
                None,
                started,
            ));
        }
    };

    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let max_response_bytes = effective_max_response_bytes(target, &options);
    let response_bytes = match read_capped_response(response, max_response_bytes).await {
        Ok(bytes) => bytes,
        Err(ReadResponseError::TooLarge { observed }) => {
            return Ok(openapi_response(
                target,
                request,
                &options,
                &tool_execution_id,
                OpenApiOutcomeStatus::ResponseTooLarge,
                Some(&plan),
                Some(status),
                Some(observed),
                started,
            ));
        }
        Err(ReadResponseError::Transport) => {
            return Ok(openapi_response(
                target,
                request,
                &options,
                &tool_execution_id,
                OpenApiOutcomeStatus::InternalError,
                Some(&plan),
                Some(status),
                None,
                started,
            ));
        }
    };

    match classify_response(status, content_type.as_deref(), &response_bytes) {
        ResponseClassification::NonSuccess { output } => Ok(openapi_response_with_output(
            target,
            request,
            &options,
            &tool_execution_id,
            OpenApiOutcomeStatus::HttpStatus(status),
            Some(&plan),
            Some(status),
            Some(response_bytes.len() as u64),
            output,
            started,
        )),
        ResponseClassification::InvalidJson => Ok(openapi_response(
            target,
            request,
            &options,
            &tool_execution_id,
            OpenApiOutcomeStatus::InvalidJsonResponse,
            Some(&plan),
            Some(status),
            Some(response_bytes.len() as u64),
            started,
        )),
        ResponseClassification::Success { output } => Ok(openapi_response_with_output(
            target,
            request,
            &options,
            &tool_execution_id,
            OpenApiOutcomeStatus::HttpStatus(status),
            Some(&plan),
            Some(status),
            Some(response_bytes.len() as u64),
            output,
            started,
        )),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ResponseClassification {
    NonSuccess { output: serde_json::Value },
    InvalidJson,
    Success { output: serde_json::Value },
}

fn classify_response(
    status: u16,
    content_type: Option<&str>,
    response_bytes: &[u8],
) -> ResponseClassification {
    if !(200..=299).contains(&status) {
        let output = serde_json::from_slice(response_bytes).unwrap_or_else(|_| {
            serde_json::json!({
                "error": {
                    "code": "openapi_upstream_error",
                    "message": String::from_utf8_lossy(response_bytes),
                }
            })
        });
        return ResponseClassification::NonSuccess { output };
    }

    if !is_json_content_type(content_type) {
        return ResponseClassification::InvalidJson;
    }

    match serde_json::from_slice(response_bytes) {
        Ok(output) => ResponseClassification::Success { output },
        Err(_) => ResponseClassification::InvalidJson,
    }
}

fn runtime_target_from_static(
    target: &AgentToolTargetConfig,
    request: &ToolCallRequest,
) -> Result<Option<RuntimeOpenApiTarget>, ToolExecutionErrorKind> {
    let Some(url) = target.url.as_deref() else {
        return Ok(None);
    };
    let parsed = Url::parse(url).map_err(|_| ToolExecutionErrorKind::ToolTargetUnavailable)?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(ToolExecutionErrorKind::ToolTargetUnavailable);
    }
    let host = parsed
        .host_str()
        .ok_or(ToolExecutionErrorKind::ToolTargetUnavailable)?;
    let port = parsed
        .port_or_known_default()
        .ok_or(ToolExecutionErrorKind::ToolTargetUnavailable)?;
    let base_url = origin_url(&parsed, host, port);
    let method = target.method.trim().to_ascii_uppercase();
    let mut runtime_target = RuntimeOpenApiTarget {
        service_slug: target
            .service_slug
            .clone()
            .unwrap_or_else(|| target.tool_id.clone()),
        operation_id: target
            .operation_id
            .clone()
            .unwrap_or_else(|| target.tool_id.clone()),
        operation_slug: target
            .operation_slug
            .clone()
            .unwrap_or_else(|| target.tool_id.clone()),
        base_url,
        canonical_host: host.to_string(),
        allowed_scheme: parsed.scheme().to_string(),
        allowed_port: port,
        path_template: parsed.path().to_string(),
        max_response_bytes: 0,
        ..RuntimeOpenApiTarget::default()
    };

    match method.as_str() {
        "GET" => {
            runtime_target.parameter_mapping = static_query_mappings(&parsed, &request.arguments);
        }
        "POST" => {
            runtime_target.request_body_mapping = Some(OpenApiBodyMapping {
                source: OpenApiValueSource {
                    literal: Some(request.arguments.clone()),
                    ..OpenApiValueSource::default()
                },
            });
        }
        _ => return Err(ToolExecutionErrorKind::ToolTargetUnavailable),
    }

    Ok(Some(runtime_target))
}

fn static_query_mappings(
    parsed: &Url,
    arguments: &serde_json::Value,
) -> Vec<OpenApiParameterMapping> {
    let mut mappings: Vec<OpenApiParameterMapping> = parsed
        .query_pairs()
        .map(|(name, value)| OpenApiParameterMapping {
            location: OpenApiParameterLocation::Query,
            name: name.into_owned(),
            source: OpenApiValueSource {
                literal: Some(serde_json::Value::String(value.into_owned())),
                ..OpenApiValueSource::default()
            },
            required: false,
        })
        .collect();
    mappings.extend(
        arguments
            .as_object()
            .into_iter()
            .flat_map(|object| object.iter())
            .filter(|(_, value)| is_scalar(value))
            .map(|(name, _)| OpenApiParameterMapping {
                location: OpenApiParameterLocation::Query,
                name: name.clone(),
                source: OpenApiValueSource {
                    argument_path: Some(format!("$.{name}")),
                    ..OpenApiValueSource::default()
                },
                required: false,
            }),
    );
    mappings
}

fn is_scalar(value: &serde_json::Value) -> bool {
    matches!(
        value,
        serde_json::Value::String(_) | serde_json::Value::Number(_) | serde_json::Value::Bool(_)
    )
}

fn origin_url(parsed: &Url, host: &str, port: u16) -> String {
    let default_port = match parsed.scheme() {
        "http" => 80,
        "https" => 443,
        _ => port,
    };
    if port == default_port {
        format!("{}://{}", parsed.scheme(), host)
    } else {
        format!("{}://{}:{}", parsed.scheme(), host, port)
    }
}

fn openapi_client(
    timeout_ms: u64,
    egress_policy: &AgentToolEgressPolicyConfig,
) -> Result<reqwest::Client, reqwest::Error> {
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_millis(timeout_ms.max(1)));
    if !egress_policy.allow_environment_proxy {
        builder = builder.no_proxy();
    }
    builder.build()
}

fn effective_timeout_ms(
    target_timeout_ms: Option<u64>,
    request_timeout_ms: Option<u64>,
    default_timeout_ms: u64,
) -> u64 {
    let configured_timeout_ms = target_timeout_ms.unwrap_or(default_timeout_ms).max(1);
    request_timeout_ms
        .map(|request_timeout_ms| request_timeout_ms.max(1).min(configured_timeout_ms))
        .unwrap_or(configured_timeout_ms)
}

fn effective_max_response_bytes(
    target: &RuntimeOpenApiTarget,
    options: &OpenApiExecutionOptions,
) -> usize {
    let configured = if target.max_response_bytes == 0 {
        options.max_response_bytes as u64
    } else {
        target
            .max_response_bytes
            .min(options.max_response_bytes as u64)
    };
    configured.max(1) as usize
}

async fn dispatch_request(
    client: &reqwest::Client,
    plan: &OpenApiRequestPlan,
) -> Result<reqwest::Response, reqwest::Error> {
    let method = reqwest::Method::from_bytes(plan.method.as_bytes())
        .expect("OpenAPI request method should be validated before dispatch");
    let mut builder = client.request(method, plan.url.clone());
    for (name, value) in &plan.headers {
        builder = builder.header(name, value);
    }
    if let Some(body) = &plan.body {
        builder = builder.json(body);
    }
    builder.send().await
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ReadResponseError {
    TooLarge { observed: u64 },
    Transport,
}

async fn read_capped_response(
    response: reqwest::Response,
    max_response_bytes: usize,
) -> Result<Vec<u8>, ReadResponseError> {
    if let Some(len) = response.content_length()
        && len > max_response_bytes as u64
    {
        return Err(ReadResponseError::TooLarge { observed: len });
    }

    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream
        .try_next()
        .await
        .map_err(|_| ReadResponseError::Transport)?
    {
        let next_len = body
            .len()
            .checked_add(chunk.len())
            .ok_or(ReadResponseError::TooLarge { observed: u64::MAX })?;
        if next_len > max_response_bytes {
            return Err(ReadResponseError::TooLarge {
                observed: next_len as u64,
            });
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn is_json_content_type(content_type: Option<&str>) -> bool {
    content_type
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .is_some_and(|value| {
            value.eq_ignore_ascii_case("application/json")
                || value.to_ascii_lowercase().ends_with("+json")
        })
}

#[allow(clippy::too_many_arguments)]
fn openapi_response(
    target: &RuntimeOpenApiTarget,
    request: &ToolCallRequest,
    options: &OpenApiExecutionOptions,
    tool_execution_id: &str,
    status: OpenApiOutcomeStatus,
    plan: Option<&OpenApiRequestPlan>,
    http_status: Option<u16>,
    response_bytes: Option<u64>,
    started: Instant,
) -> ToolCallResponse {
    openapi_response_with_output(
        target,
        request,
        options,
        tool_execution_id,
        status,
        plan,
        http_status,
        response_bytes,
        serde_json::json!({
            "error": null,
            "target_kind": "openapi",
        }),
        started,
    )
}

#[allow(clippy::too_many_arguments)]
fn openapi_response_with_output(
    target: &RuntimeOpenApiTarget,
    request: &ToolCallRequest,
    options: &OpenApiExecutionOptions,
    tool_execution_id: &str,
    status: OpenApiOutcomeStatus,
    plan: Option<&OpenApiRequestPlan>,
    http_status: Option<u16>,
    response_bytes: Option<u64>,
    output: serde_json::Value,
    started: Instant,
) -> ToolCallResponse {
    let decision = decide(OpenApiOutcomeInput {
        status,
        fixed_micros: options.fixed_micros,
        currency: options.currency.clone(),
        charge_on_failure: options.charge_on_failure,
        tool_execution_id: tool_execution_id.to_string(),
    });
    let failure_class = decision
        .error
        .as_ref()
        .map(|error| error.code.clone())
        .or_else(|| {
            if decision.status == crate::agent::tools::types::ToolExecutionStatus::Completed {
                None
            } else {
                Some(decision.billing_reason.clone())
            }
        });
    let cost_source = if decision.billing.billable {
        "rate_card"
    } else {
        "waived"
    };
    let cost_micros = decision.billing.cost_micros;

    ToolCallResponse {
        status: decision.status,
        tool_call_id: request.tool_call_id.clone(),
        tool_execution_id: tool_execution_id.to_string(),
        output,
        error: decision.error,
        gateway_metadata: Some(ToolGatewayMetadata {
            execution_source: "gateway_executed".to_string(),
            target_kind: "openapi".to_string(),
            target_id: options.target_id.clone(),
            target_hash: target.target_hash.clone(),
            auth_revision: format!("{}/runtime", target.auth_revision),
            cache_hit: false,
            reinitialized: false,
            protocol_version: None,
            sse_used: false,
            failure_class,
            blocked_before_dispatch: matches!(
                status,
                OpenApiOutcomeStatus::SchemaInvalid
                    | OpenApiOutcomeStatus::PolicyBlocked
                    | OpenApiOutcomeStatus::EgressBlocked
                    | OpenApiOutcomeStatus::RequestTooLarge
                    | OpenApiOutcomeStatus::SnapshotStale
            ),
            latency_ms: Some(started.elapsed().as_millis() as u64),
            service_slug: non_empty(target.service_slug.clone()),
            operation_id: non_empty(target.operation_id.clone()),
            operation_slug: non_empty(target.operation_slug.clone()),
            http_method: plan.map(|plan| plan.method.clone()),
            http_status,
            request_bytes: plan.map(|plan| plan.request_bytes),
            response_bytes,
            billing_status: Some(decision.billing_status),
            billing_reason: Some(decision.billing_reason),
            executed: Some(decision.executed),
            failure_stage: Some(decision.failure_stage),
            ..ToolGatewayMetadata::default()
        }),
        billing: decision.billing,
        cost: ToolCost {
            amount_micros: cost_micros,
            currency: options.currency.clone(),
            source: cost_source.to_string(),
        },
        policy: ToolPolicySummary {
            allowed: true,
            decision: "allowed".to_string(),
            reason: "tool_allowed".to_string(),
        },
        events: ToolExecutionEvents::default(),
    }
}

fn non_empty(value: String) -> Option<String> {
    if value.is_empty() { None } else { Some(value) }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::{TcpListener, TcpStream},
        sync::mpsc,
        thread,
        time::Duration,
    };

    use super::*;
    use crate::{
        agent::tools::{openapi::types::RuntimeOpenApiTarget, types::ToolExecutionStatus},
        config::agent::{
            AgentToolEgressPolicyConfig, AgentToolRateCardConfig, AgentToolTargetKind,
        },
    };

    const DEFAULT_TIMEOUT_MS: u64 = 8_000;
    const MAX_REQUEST_BYTES: usize = 65_536;
    const MAX_RESPONSE_BYTES: usize = 65_536;

    #[tokio::test]
    async fn get_2xx_json_response_completes_with_metadata() {
        let Some(fixture) = OpenApiHttpFixture::start(FixtureResponse {
            status: 200,
            content_type: "application/json",
            body: r#"{"ok":true}"#,
            delay: None,
            location: None,
        }) else {
            return;
        };
        let target = static_target(fixture.url("/tickets"));

        let response = execute_openapi_tool(
            &target,
            &request(serde_json::json!({ "ticket_id": "T-1" })),
            &test_egress_policy(),
            DEFAULT_TIMEOUT_MS,
            MAX_REQUEST_BYTES,
            MAX_RESPONSE_BYTES,
        )
        .await
        .expect("static OpenAPI target executes");

        assert_eq!(response.status, ToolExecutionStatus::Completed);
        assert_eq!(response.output, serde_json::json!({ "ok": true }));
        assert_eq!(response.error, None);
        assert_eq!(response.billing.reason, "openapi_2xx");
        let metadata = response
            .gateway_metadata
            .expect("gateway metadata should exist");
        assert_eq!(metadata.target_kind, "openapi");
        assert_eq!(metadata.http_status, Some(200));
        assert!(metadata.request_bytes.unwrap_or_default() > 0);
        assert_eq!(metadata.response_bytes, Some(11));
        assert_eq!(metadata.billing_status.as_deref(), Some("actual"));
        assert_eq!(metadata.billing_reason.as_deref(), Some("openapi_2xx"));
        assert_eq!(metadata.executed, Some(true));
        assert_eq!(metadata.failure_stage.as_deref(), Some(""));

        let observed = fixture.receive_request();
        assert!(observed.starts_with("GET /tickets?ticket_id=T-1 "));
    }

    #[tokio::test]
    async fn static_get_preserves_literal_url_query_parameters() {
        let Some(fixture) = OpenApiHttpFixture::start(FixtureResponse {
            status: 200,
            content_type: "application/json",
            body: r#"{"ok":true}"#,
            delay: None,
            location: None,
        }) else {
            return;
        };
        let target = static_target(fixture.url("/tickets?environment=prod"));

        let response = execute_openapi_tool(
            &target,
            &request(serde_json::json!({ "ticket_id": "T-1" })),
            &test_egress_policy(),
            DEFAULT_TIMEOUT_MS,
            MAX_REQUEST_BYTES,
            MAX_RESPONSE_BYTES,
        )
        .await
        .expect("static OpenAPI target executes");

        assert_eq!(response.status, ToolExecutionStatus::Completed);
        let observed = fixture.receive_request();
        assert!(
            observed.starts_with("GET /tickets?environment=prod&ticket_id=T-1 "),
            "{observed}"
        );
    }

    #[tokio::test]
    async fn runtime_404_can_be_failed_and_billable() {
        let Some(fixture) = OpenApiHttpFixture::start(FixtureResponse {
            status: 404,
            content_type: "application/json",
            body: r#"{"error":"missing"}"#,
            delay: None,
            location: None,
        }) else {
            return;
        };
        let runtime = runtime_target(fixture.url("/tickets/T-404"), 65_536);

        let response = execute_runtime_openapi_tool(
            &runtime,
            "GET",
            &request(serde_json::json!({})),
            &test_egress_policy(),
            OpenApiExecutionOptions {
                target_id: "support.get-ticket".to_string(),
                fixed_micros: 77,
                currency: "USD".to_string(),
                charge_on_failure: true,
                timeout_ms: DEFAULT_TIMEOUT_MS,
                max_request_bytes: MAX_REQUEST_BYTES,
                max_response_bytes: MAX_RESPONSE_BYTES,
            },
        )
        .await
        .expect("404 maps to response envelope");

        assert_eq!(response.status, ToolExecutionStatus::Failed);
        assert_eq!(
            response.error.as_ref().map(|error| error.code.as_str()),
            Some("openapi_http_4xx")
        );
        assert!(response.billing.billable);
        assert_eq!(response.billing.cost_micros, 77);
        assert_eq!(response.billing.reason, "openapi_4xx_per_call");
        let metadata = response.gateway_metadata.unwrap();
        assert_eq!(metadata.http_status, Some(404));
        assert_eq!(
            metadata.billing_reason.as_deref(),
            Some("openapi_4xx_per_call")
        );
        assert_eq!(metadata.failure_stage.as_deref(), Some("upstream"));
    }

    #[tokio::test]
    async fn runtime_404_text_response_keeps_http_status_classification() {
        let Some(fixture) = OpenApiHttpFixture::start(FixtureResponse {
            status: 404,
            content_type: "text/html",
            body: "<h1>missing</h1>",
            delay: None,
            location: None,
        }) else {
            return;
        };
        let runtime = runtime_target(fixture.url("/tickets/T-404"), 65_536);

        let response = execute_runtime_openapi_tool(
            &runtime,
            "GET",
            &request(serde_json::json!({})),
            &test_egress_policy(),
            OpenApiExecutionOptions {
                target_id: "support.get-ticket".to_string(),
                fixed_micros: 77,
                currency: "USD".to_string(),
                charge_on_failure: true,
                timeout_ms: DEFAULT_TIMEOUT_MS,
                max_request_bytes: MAX_REQUEST_BYTES,
                max_response_bytes: MAX_RESPONSE_BYTES,
            },
        )
        .await
        .expect("404 text maps to HTTP status response envelope");

        assert_eq!(response.status, ToolExecutionStatus::Failed);
        assert_eq!(
            response.error.as_ref().map(|error| error.code.as_str()),
            Some("openapi_http_4xx")
        );
        assert_eq!(response.billing.reason, "openapi_4xx_per_call");
        assert_eq!(response.gateway_metadata.unwrap().http_status, Some(404));
    }

    #[tokio::test]
    async fn upstream_503_returns_failed_response() {
        let Some(fixture) = OpenApiHttpFixture::start(FixtureResponse {
            status: 503,
            content_type: "application/json",
            body: r#"{"error":"down"}"#,
            delay: None,
            location: None,
        }) else {
            return;
        };
        let target = static_target(fixture.url("/tickets"));

        let response = execute_openapi_tool(
            &target,
            &request(serde_json::json!({})),
            &test_egress_policy(),
            DEFAULT_TIMEOUT_MS,
            MAX_REQUEST_BYTES,
            MAX_RESPONSE_BYTES,
        )
        .await
        .expect("503 maps to response envelope");

        assert_eq!(response.status, ToolExecutionStatus::Failed);
        assert_eq!(
            response.error.as_ref().map(|error| error.code.as_str()),
            Some("openapi_http_5xx")
        );
        assert_eq!(response.gateway_metadata.unwrap().http_status, Some(503));
    }

    #[tokio::test]
    async fn upstream_503_invalid_json_keeps_http_status_classification() {
        let Some(fixture) = OpenApiHttpFixture::start(FixtureResponse {
            status: 503,
            content_type: "application/json",
            body: "not-json",
            delay: None,
            location: None,
        }) else {
            return;
        };
        let target = static_target(fixture.url("/tickets"));

        let response = execute_openapi_tool(
            &target,
            &request(serde_json::json!({})),
            &test_egress_policy(),
            DEFAULT_TIMEOUT_MS,
            MAX_REQUEST_BYTES,
            MAX_RESPONSE_BYTES,
        )
        .await
        .expect("503 invalid JSON maps to HTTP status response envelope");

        assert_eq!(response.status, ToolExecutionStatus::Failed);
        assert_eq!(
            response.error.as_ref().map(|error| error.code.as_str()),
            Some("openapi_http_5xx")
        );
        assert_eq!(response.gateway_metadata.unwrap().http_status, Some(503));
    }

    #[test]
    fn non_success_invalid_json_is_classified_before_json_validation() {
        let classification = classify_response(503, Some("application/json"), b"not-json");

        assert!(matches!(
            classification,
            ResponseClassification::NonSuccess { .. }
        ));
    }

    #[test]
    fn success_invalid_json_is_classified_as_invalid_json_response() {
        let classification = classify_response(200, Some("application/json"), b"not-json");

        assert_eq!(classification, ResponseClassification::InvalidJson);
    }

    #[tokio::test]
    async fn timeout_returns_timeout_response() {
        let Some(fixture) = OpenApiHttpFixture::start(FixtureResponse {
            status: 200,
            content_type: "application/json",
            body: r#"{"ok":true}"#,
            delay: Some(Duration::from_millis(100)),
            location: None,
        }) else {
            return;
        };
        let mut target = static_target(fixture.url("/slow"));
        target.timeout_ms = Some(10);

        let response = execute_openapi_tool(
            &target,
            &request(serde_json::json!({})),
            &test_egress_policy(),
            DEFAULT_TIMEOUT_MS,
            MAX_REQUEST_BYTES,
            MAX_RESPONSE_BYTES,
        )
        .await
        .expect("timeout maps to response envelope");

        assert_eq!(response.status, ToolExecutionStatus::Timeout);
        assert_eq!(
            response.error.as_ref().map(|error| error.code.as_str()),
            Some("openapi_timeout")
        );
        assert_eq!(
            response.gateway_metadata.unwrap().failure_stage.as_deref(),
            Some("timeout")
        );
    }

    #[tokio::test]
    async fn response_over_byte_cap_returns_failed_response() {
        let Some(fixture) = OpenApiHttpFixture::start(FixtureResponse {
            status: 200,
            content_type: "application/json",
            body: r#"{"ok":true}"#,
            delay: None,
            location: None,
        }) else {
            return;
        };
        let runtime = runtime_target(fixture.url("/tickets"), 4);

        let response = execute_runtime_openapi_tool(
            &runtime,
            "GET",
            &request(serde_json::json!({})),
            &test_egress_policy(),
            OpenApiExecutionOptions {
                target_id: "support.get-ticket".to_string(),
                fixed_micros: 77,
                currency: "USD".to_string(),
                charge_on_failure: false,
                timeout_ms: DEFAULT_TIMEOUT_MS,
                max_request_bytes: MAX_REQUEST_BYTES,
                max_response_bytes: MAX_RESPONSE_BYTES,
            },
        )
        .await
        .expect("oversized response maps to response envelope");

        assert_eq!(response.status, ToolExecutionStatus::Failed);
        assert_eq!(
            response.error.as_ref().map(|error| error.code.as_str()),
            Some("openapi_response_too_large")
        );
        assert_eq!(
            response.gateway_metadata.unwrap().failure_stage.as_deref(),
            Some("response")
        );
    }

    #[tokio::test]
    async fn static_target_uses_unified_response_byte_cap() {
        let large_json: &'static str =
            Box::leak(format!(r#"{{"payload":"{}"}}"#, "a".repeat(70 * 1024)).into_boxed_str());
        let Some(fixture) = OpenApiHttpFixture::start(FixtureResponse {
            status: 200,
            content_type: "application/json",
            body: large_json,
            delay: None,
            location: None,
        }) else {
            return;
        };
        let target = static_target(fixture.url("/tickets"));

        let response = execute_openapi_tool(
            &target,
            &request(serde_json::json!({})),
            &test_egress_policy(),
            DEFAULT_TIMEOUT_MS,
            MAX_REQUEST_BYTES,
            128 * 1024,
        )
        .await
        .expect("static target uses unified response cap");

        assert_eq!(response.status, ToolExecutionStatus::Completed);
        assert_eq!(
            response.output["payload"].as_str().unwrap().len(),
            70 * 1024
        );
    }

    #[tokio::test]
    async fn non_json_response_returns_invalid_json_response() {
        let Some(fixture) = OpenApiHttpFixture::start(FixtureResponse {
            status: 200,
            content_type: "text/plain",
            body: "not json",
            delay: None,
            location: None,
        }) else {
            return;
        };
        let target = static_target(fixture.url("/plain"));

        let response = execute_openapi_tool(
            &target,
            &request(serde_json::json!({})),
            &test_egress_policy(),
            DEFAULT_TIMEOUT_MS,
            MAX_REQUEST_BYTES,
            MAX_RESPONSE_BYTES,
        )
        .await
        .expect("non-json maps to response envelope");

        assert_eq!(response.status, ToolExecutionStatus::Failed);
        assert_eq!(
            response.error.as_ref().map(|error| error.code.as_str()),
            Some("openapi_invalid_json_response")
        );
    }

    #[tokio::test]
    async fn redirect_response_is_not_followed() {
        let Some(fixture) = OpenApiHttpFixture::start(FixtureResponse {
            status: 302,
            content_type: "application/json",
            body: r#"{"redirect":true}"#,
            delay: None,
            location: Some("/final"),
        }) else {
            return;
        };
        let target = static_target(fixture.url("/redirect"));

        let response = execute_openapi_tool(
            &target,
            &request(serde_json::json!({})),
            &test_egress_policy(),
            DEFAULT_TIMEOUT_MS,
            MAX_REQUEST_BYTES,
            MAX_RESPONSE_BYTES,
        )
        .await
        .expect("redirect maps to response envelope");

        assert_eq!(response.status, ToolExecutionStatus::Failed);
        assert_eq!(response.gateway_metadata.unwrap().http_status, Some(302));
        let observed = fixture.receive_request();
        assert!(observed.starts_with("GET /redirect "));
    }

    #[tokio::test]
    async fn invalid_runtime_method_fails_before_dispatch() {
        let Some(fixture) = OpenApiHttpFixture::start(FixtureResponse {
            status: 200,
            content_type: "application/json",
            body: r#"{"ok":true}"#,
            delay: None,
            location: None,
        }) else {
            return;
        };
        let runtime = runtime_target(fixture.url("/tickets"), 65_536);

        let response = execute_runtime_openapi_tool(
            &runtime,
            "BAD METHOD",
            &request(serde_json::json!({})),
            &test_egress_policy(),
            OpenApiExecutionOptions {
                target_id: "support.get-ticket".to_string(),
                fixed_micros: 77,
                currency: "USD".to_string(),
                charge_on_failure: false,
                timeout_ms: DEFAULT_TIMEOUT_MS,
                max_request_bytes: MAX_REQUEST_BYTES,
                max_response_bytes: MAX_RESPONSE_BYTES,
            },
        )
        .await
        .expect("invalid method maps to response envelope");

        assert_eq!(response.status, ToolExecutionStatus::Failed);
        assert_eq!(
            response.error.as_ref().map(|error| error.code.as_str()),
            Some("openapi_schema_invalid")
        );
        assert!(fixture.try_receive_request().is_none());
    }

    #[tokio::test]
    async fn request_over_byte_cap_fails_before_dispatch() {
        let mut runtime = runtime_target("https://api.example.test/tickets".to_string(), 65_536);
        runtime.parameter_mapping = vec![OpenApiParameterMapping {
            location: OpenApiParameterLocation::Query,
            name: "query".to_string(),
            source: OpenApiValueSource {
                argument_path: Some("$.query".to_string()),
                literal: None,
                secret_ref: None,
            },
            required: true,
        }];

        let response = execute_runtime_openapi_tool(
            &runtime,
            "GET",
            &request(serde_json::json!({
                "query": "this request is intentionally too large"
            })),
            &test_egress_policy(),
            OpenApiExecutionOptions {
                target_id: "support.get-ticket".to_string(),
                fixed_micros: 77,
                currency: "USD".to_string(),
                charge_on_failure: true,
                timeout_ms: DEFAULT_TIMEOUT_MS,
                max_request_bytes: 8,
                max_response_bytes: MAX_RESPONSE_BYTES,
            },
        )
        .await
        .expect("oversized request maps to response envelope");

        assert_eq!(response.status, ToolExecutionStatus::Failed);
        assert_eq!(
            response.error.as_ref().map(|error| error.code.as_str()),
            Some("openapi_request_too_large")
        );
        let metadata = response.gateway_metadata.unwrap();
        assert_eq!(metadata.executed, Some(false));
        assert!(metadata.blocked_before_dispatch);
        assert_eq!(metadata.failure_stage.as_deref(), Some("request"));
        assert!(!response.billing.billable);
    }

    fn static_target(url: String) -> AgentToolTargetConfig {
        AgentToolTargetConfig {
            tool_id: "support.get-ticket".to_string(),
            kind: AgentToolTargetKind::OpenApi,
            url: Some(url),
            method: "GET".to_string(),
            rate_card: AgentToolRateCardConfig {
                fixed_micros: 77,
                currency: "USD".to_string(),
            },
            ..AgentToolTargetConfig::default()
        }
    }

    fn runtime_target(url: String, max_response_bytes: u64) -> RuntimeOpenApiTarget {
        let parsed = url::Url::parse(&url).expect("fixture URL should parse");
        RuntimeOpenApiTarget {
            base_url: format!(
                "{}://{}:{}",
                parsed.scheme(),
                parsed.host_str().expect("fixture URL should have host"),
                parsed
                    .port_or_known_default()
                    .expect("fixture URL should have port")
            ),
            canonical_host: parsed
                .host_str()
                .expect("fixture URL should have host")
                .to_string(),
            allowed_scheme: parsed.scheme().to_string(),
            allowed_port: parsed
                .port_or_known_default()
                .expect("fixture URL should have port"),
            path_template: parsed.path().to_string(),
            max_response_bytes,
            ..RuntimeOpenApiTarget::default()
        }
    }

    fn request(arguments: serde_json::Value) -> ToolCallRequest {
        ToolCallRequest {
            tool_id: "support.get-ticket".to_string(),
            tool_call_id: Some("call-openapi".to_string()),
            tool_execution_id: Some("exec-openapi".to_string()),
            arguments,
            ..ToolCallRequest::default()
        }
    }

    fn test_egress_policy() -> AgentToolEgressPolicyConfig {
        AgentToolEgressPolicyConfig {
            https_only: false,
            block_loopback: false,
            block_link_local: false,
            block_metadata_ip: false,
            block_private_network: false,
            allow_environment_proxy: false,
        }
    }

    struct FixtureResponse {
        status: u16,
        content_type: &'static str,
        body: &'static str,
        delay: Option<Duration>,
        location: Option<&'static str>,
    }

    struct OpenApiHttpFixture {
        base_url: String,
        receiver: mpsc::Receiver<String>,
    }

    impl OpenApiHttpFixture {
        fn start(response: FixtureResponse) -> Option<Self> {
            let listener = match TcpListener::bind("127.0.0.1:0") {
                Ok(listener) => listener,
                Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
                    eprintln!("skipping OpenAPI HTTP loopback fixture: {err}");
                    return None;
                }
                Err(err) => panic!("bind OpenAPI test server: {err}"),
            };
            let addr = listener.local_addr().expect("test server addr");
            let (tx, rx) = mpsc::channel();
            thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept OpenAPI test request");
                let request = read_http_request(&mut stream);
                tx.send(request).expect("send observed request");
                if let Some(delay) = response.delay {
                    thread::sleep(delay);
                }
                let status_line = match response.status {
                    200..=299 => format!("HTTP/1.1 {} OK", response.status),
                    300..=399 => {
                        format!("HTTP/1.1 {} Redirect", response.status)
                    }
                    _ => format!("HTTP/1.1 {} Error", response.status),
                };
                let location = response
                    .location
                    .map(|location| format!("Location: {location}\r\n"))
                    .unwrap_or_default();
                let raw = format!(
                    "{status_line}\r\n{location}Content-Type: \
                     {}\r\nContent-Length: {}\r\n\r\n{}",
                    response.content_type,
                    response.body.len(),
                    response.body
                );
                let _ = stream.write_all(raw.as_bytes());
            });

            Some(Self {
                base_url: format!("http://{addr}"),
                receiver: rx,
            })
        }

        fn url(&self, path: &str) -> String {
            format!("{}{}", self.base_url, path)
        }

        fn receive_request(self) -> String {
            self.receiver.recv().expect("observed request")
        }

        fn try_receive_request(self) -> Option<String> {
            self.receiver.recv_timeout(Duration::from_millis(25)).ok()
        }
    }

    fn read_http_request(stream: &mut TcpStream) -> String {
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            let read = stream.read(&mut buffer).expect("read request");
            if read == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..read]);
            if request_headers_complete(&bytes) {
                break;
            }
        }
        String::from_utf8_lossy(&bytes).to_string()
    }

    fn request_headers_complete(bytes: &[u8]) -> bool {
        bytes.windows(4).any(|window| window == b"\r\n\r\n")
    }
}
