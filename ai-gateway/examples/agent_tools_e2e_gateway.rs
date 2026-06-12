#[cfg(feature = "testing")]
use std::{convert::Infallible, error::Error, net::IpAddr, path::PathBuf};

#[cfg(feature = "testing")]
use ai_gateway::{
    agent::tools::service::{AgentToolsService, prepare_e2e_request},
    app::build_test_app,
    config::Config,
    store::router::DbVirtualKey,
    types::{extensions::AuthContext, org::OrgId, secret::Secret, user::UserId},
    virtual_key::legacy_key::hash_key,
};
#[cfg(feature = "testing")]
use chrono::Utc;
#[cfg(feature = "testing")]
use clap::Parser;
#[cfg(feature = "testing")]
use http::{Request, StatusCode};
#[cfg(feature = "testing")]
use hyper::body::Incoming;
#[cfg(feature = "testing")]
use rustc_hash::FxHashMap;
#[cfg(feature = "testing")]
use tower::{Service, ServiceExt, make::Shared, service_fn};
#[cfg(feature = "testing")]
use uuid::Uuid;

#[cfg(feature = "testing")]
const DEFAULT_API_KEY: &str = "sk-agent-tools-e2e";

#[cfg(feature = "testing")]
#[derive(Debug, Parser)]
struct Args {
    #[arg(
        short,
        long,
        default_value = "examples/agent/tools/e2e.agent-tools.yaml"
    )]
    config: PathBuf,
    #[arg(long, default_value = "127.0.0.1")]
    host: IpAddr,
    #[arg(short, long, default_value_t = 18080)]
    port: u16,
    #[arg(long, default_value = DEFAULT_API_KEY)]
    api_key: String,
}

#[cfg(feature = "testing")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok();

    let args = Args::parse();
    let mut config = Config::try_read(Some(&args.config))?;
    config.server.address = args.host;
    config.server.port = args.port;
    config.compat_mode = true;
    let app = build_test_app(config).await?;
    let auth_context = seed_virtual_key(&app.state, &args.api_key).await;
    app.state.mark_cache_warmed();

    let addr = std::net::SocketAddr::from((args.host, args.port));
    println!("agent tools e2e gateway listening on http://{addr}");
    println!("api key: {api_key}", api_key = args.api_key);

    let agent_tools = AgentToolsService::new(app.state.clone());
    let expected_auth = format!("Bearer {}", args.api_key);
    let service = service_fn(move |request: Request<Incoming>| {
        let mut agent_tools = agent_tools.clone();
        let auth_context = auth_context.clone();
        let expected_auth = expected_auth.clone();
        async move {
            if request
                .headers()
                .get(http::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                != Some(expected_auth.as_str())
            {
                return Ok::<_, Infallible>(
                    http::Response::builder()
                        .status(StatusCode::UNAUTHORIZED)
                        .body(axum_core::body::Body::empty())
                        .expect("valid response"),
                );
            }
            let action = match request.uri().path() {
                "/v1/agent/tools/list" => "list",
                "/v1/agent/tools/call" => "call",
                _ => {
                    return Ok::<_, Infallible>(
                        http::Response::builder()
                            .status(StatusCode::NOT_FOUND)
                            .body(axum_core::body::Body::empty())
                            .expect("valid response"),
                    );
                }
            };
            let request = request.map(axum_core::body::Body::new);
            let request = prepare_e2e_request(request, action, auth_context);
            agent_tools.ready().await.map_err(|err| match err {})?;
            let response = agent_tools
                .call(request)
                .await
                .map_err(|err| match err {})?;
            Ok::<_, Infallible>(response)
        }
    });

    axum_server::bind(addr).serve(Shared::new(service)).await?;
    Ok(())
}

#[cfg(not(feature = "testing"))]
fn main() {
    eprintln!("agent_tools_e2e_gateway requires --features testing,external");
    std::process::exit(1);
}

#[cfg(feature = "testing")]
async fn seed_virtual_key(
    app_state: &ai_gateway::app_state::AppState,
    api_key: &str,
) -> AuthContext {
    let workspace_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let virtual_key_id = Uuid::new_v4();
    let master_key_id = Uuid::new_v4();
    let virtual_key = DbVirtualKey {
        id: virtual_key_id,
        workspace_id,
        master_key_id,
        key_hash: hash_key(api_key),
        key_prefix: "e2e-agent-tools".to_string(),
        label: "agent:Agent Tools E2E".to_string(),
        entity_type: Some("agent".to_string()),
        entity_id: Some(agent_id),
        status: "active".to_string(),
        expires_at: None,
        deleted_at: None,
        updated_at: Utc::now(),
        rate_limit_rpm: None,
        rate_limit_rph: None,
        allowed_models: None,
        blocked_models: None,
        subscription_log_limit: 90,
    };

    let mut cache = app_state.0.virtual_keys_cache.write().await;
    let mut map = FxHashMap::default();
    map.insert(virtual_key.key_hash.clone(), virtual_key);
    *cache = Some(map);

    AuthContext {
        api_key: Secret::from(api_key.to_string()),
        user_id: UserId::new(agent_id),
        org_id: OrgId::new(workspace_id),
        workspace_type: Some("e2e".to_string()),
        virtual_key_id: Some(virtual_key_id),
        virtual_key_prefix: "e2e-agent-tools".to_string(),
        master_key_id: Some(master_key_id),
        master_key_base_url: None,
        department_id: Uuid::nil(),
        entity_type: "agent".to_string(),
        entity_id: agent_id,
        entity_name: "agent:Agent Tools E2E".to_string(),
        registered_agent_name: Some("Agent Tools E2E".to_string()),
        body_ttl_days: 90,
        is_custom_provider: false,
        master_key_allowed_providers: None,
    }
}
