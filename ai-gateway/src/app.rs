use std::{
    convert::Infallible,
    future::{Ready, ready},
    net::SocketAddr,
    sync::{Arc, atomic::AtomicBool},
    task::{Context, Poll},
};

use axum_server::{accept::NoDelayAcceptor, tls_rustls::RustlsConfig};
use futures::future::BoxFuture;
use meltdown::Token;
use opentelemetry::global;
use rustc_hash::FxHashMap as HashMap;
use telemetry::{make_span::SpanFactory, tracing::MakeRequestId};
use tokio::sync::RwLock;
use tower::{ServiceBuilder, buffer::BufferLayer, util::BoxCloneService};
use tower_http::{
    ServiceBuilderExt,
    add_extension::AddExtension,
    catch_panic::CatchPanicLayer,
    compression::CompressionLayer,
    cors::{Any, CorsLayer},
    normalize_path::NormalizePathLayer,
    sensitive_headers::SetSensitiveHeadersLayer,
    trace::TraceLayer,
};
use tracing::{Level, info};

use crate::{
    app_state::{AppState, InnerAppState},
    cli,
    config::{Config, server::TlsConfig},
    discover::monitor::{
        health::provider::HealthMonitorMap, metrics::EndpointMetricsRegistry,
        rate_limit::RateLimitMonitorMap,
    },
    error::{init::InitError, runtime::RuntimeError},
    logger::service::AlephantHttpClient,
    metrics::{self, attribute_extractor::AttributeExtractor},
    middleware::{response_headers::ResponseHeaderLayer, security::SecurityLayer},
    plugin::PluginLoader,
    router::meta::MetaRouter,
    semantic_cache::{
        EmbeddingBaseUrlResolver, OpenAiEmbedderClient, QdrantStore, SemanticCacheService,
    },
    store::{connect, router::RouterStore, s3::BaseS3Client},
    types::provider::{InferenceProvider, ProviderKeys},
    utils::{
        catch_panic::PanicResponder, handle_error::ErrorHandlerLayer,
        health_check::HealthCheckLayer, timer::TimerLayer,
        validate_config::ValidateRouterConfigLayer,
    },
};

const APP_BUFFER_SIZE: usize = 1024;
const SERVICE_NAME: &str = "ai-gateway";

pub type AppResponseBody = tower_http::body::UnsyncBoxBody<
    bytes::Bytes,
    Box<dyn std::error::Error + std::marker::Send + std::marker::Sync + 'static>,
>;
pub type AppResponse = http::Response<AppResponseBody>;

pub type BoxedServiceStack =
    BoxCloneService<crate::types::request::Request, AppResponse, Infallible>;

pub type BoxedHyperServiceStack =
    BoxCloneService<http::Request<hyper::body::Incoming>, AppResponse, Infallible>;

/// The top level app used to start the hyper server.
/// The middleware stack is as follows:
/// -- global --
/// 0. `CatchPanic`
/// 1. `HandleError`
/// 2. Authn/Authz
/// 2.5 Fallback Request Log (catches errors from inner layers, emits log
///     for authenticated requests that no precise path logged)
/// 3. Unauthenticated and authenticated rate limit layers
/// 4. `MetaRouter`
///
/// -- Router specific MW, must not require Clone on inner Service --
/// 5. Per User Rate Limit layer
/// 6. Per Org Rate Limit layer
/// 7. `RequestContext`
///    - Fetch dynamic request specific metadata
///    - Deserialize request body based on default provider
///    - Parse Alephant inputs
/// 8. Per model rate limit layer
///    - Based on request context, rate limit based on deserialized model target
///      from request context
/// 9. Spend controls
/// 10. A/B testing between models and prompt versions
/// 11. Fallbacks
/// 12. `ProviderBalancer`
///
/// -- provider specific middleware --
/// 13. Per provider rate limit layer
/// 14. Mapper
///     - based on selected provider, map request body
/// 15. `ProviderRegionBalancer`
///
/// -- region specific middleware (none yet, just leaf service) --
/// 16. Dispatcher
///
/// For request processing, we need to use some dynamically added
/// request extensions. We try to aggregate most of this into the
/// `RequestContext` struct to keep things simple but for some things
/// we will use separate types to avoid needing to use `Option`s in
/// the `RequestContext` struct.
///
/// Required request extensions:
/// - `AuthContext`
///    - Added by the auth layer
///    - Removed by the request context layer and aggregated into the
///      `Arc<RequestContext>`
/// - `PathAndQuery`
///   - Added by the `MetaRouter`
///   - Used by the Mapper layer
/// - `ApiEndpoint`
///   - Added by the `Router`
///   - Used by the Mapper layer
/// - `Arc<RequestContext>`
///   - Added by the request context layer
///   - Used by many layers
/// - `RouterConfig`
///   - Added by the request context layer
///   - Used by the Mapper layer
/// - `MapperContext`
///   - Added by the `Mapper` layer
///   - Used by the Dispatcher layer
/// - `Provider`
///   - Added by the `AddExtensionLayer` in the dispatcher service stack
///   - Value is driven by the `Key` type used by the `Discovery` impl.
///   - Used by the Mapper layer
///
/// Required response extensions:
/// - Copied by the dispatcher from req to resp extensions
///   - `InferenceProvider`
///   - `Model`
///   - `RouterId`
///   - `PathAndQuery`
///   - `ApiEndpoint`
///   - `MapperContext`
///   - `AuthContext`
///   - `ProviderRequestId`
#[derive(Clone)]
pub struct App {
    pub state: AppState,
    pub service_stack: BoxedServiceStack,
}

impl tower::Service<crate::types::request::Request> for App {
    type Response = AppResponse;
    type Error = Infallible;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    #[inline]
    #[tracing::instrument(skip_all)]
    fn poll_ready(&mut self, ctx: &mut Context<'_>) -> std::task::Poll<Result<(), Self::Error>> {
        self.service_stack.poll_ready(ctx)
    }

    #[inline]
    fn call(&mut self, req: crate::types::request::Request) -> Self::Future {
        tracing::trace!(uri = %req.uri(), method = %req.method(), version = ?req.version(), "app received request");
        self.service_stack.call(req)
    }
}

impl App {
    pub async fn new(config: Config) -> Result<Self, InitError> {
        tracing::debug!("creating app");
        crate::config::client_ip_rate_limit::validate_global_client_ip_rate_limit(&config.global)?;
        crate::config::gateway_in_flight_limit::validate_global_gateway_in_flight_limit(
            &config.global,
        )?;
        let app_state = Self::build_app_state(config).await?;
        let service_stack = Self::build_service_stack(app_state.clone()).await?;

        if let (Some(broadcast_cfg), Some(redis_url)) = (
            app_state
                .config()
                .global
                .health_event_broadcast
                .as_ref()
                .filter(|c| c.enabled),
            app_state.config().request_log.log_queue_redis_url.as_ref(),
        ) {
            let channel = broadcast_cfg.channel.clone();
            let url = redis_url.clone();
            tokio::spawn(crate::discover::monitor::health::broadcast::run_subscriber(
                url, channel,
            ));
        }

        let app = Self {
            state: app_state,
            service_stack,
        };

        Ok(app)
    }

    /// Initializes all the clients, managers, and other stateful components
    /// that are shared across the application. This includes setting up
    /// metrics, monitoring, caching, and API keys.
    #[allow(clippy::too_many_lines)]
    async fn build_app_state(config: Config) -> Result<AppState, InitError> {
        let s3 = BaseS3Client::new(config.s3.clone())?;
        let alephant_http_client = AlephantHttpClient::new()?;

        let meter = global::meter(SERVICE_NAME);
        let metrics = metrics::Metrics::new(&meter);
        let endpoint_metrics = EndpointMetricsRegistry::new(&config);
        let health_monitor = HealthMonitorMap::default();
        let rate_limit_monitor = RateLimitMonitorMap::default();

        let router_store = if config.compat_mode {
            tracing::info!("compat_mode: skipping PostgreSQL connection");
            None
        } else {
            let pg_pool = connect(&config.database).await?;
            Some(RouterStore::new(pg_pool)?)
        };

        let alephant_api_keys = if let Some(ref rs) = router_store {
            let keys = rs
                .get_all_virtual_keys()
                .await
                .map_err(|e| InitError::InitAlephantKeys(e.to_string()))?;
            tracing::info!("loaded initial {} alephant api keys", keys.len());
            metrics
                .routers
                .alephant_api_keys
                .add(i64::try_from(keys.len()).unwrap_or(i64::MAX), &[]);
            RwLock::new(Some(keys))
        } else {
            RwLock::new(None)
        };

        let provider_keys = ProviderKeys::new(&config, &metrics);

        let master_key_encryption_key = if config.compat_mode {
            None
        } else {
            let key = crate::crypto::master_key_config::load_master_key_encryption_key()?;
            tracing::info!("MASTER_KEY_ENCRYPTION_KEY loaded successfully");
            Some(key)
        };

        let virtual_keys_cache = if let Some(ref rs) = router_store {
            let rows = rs
                .get_all_db_virtual_keys()
                .await
                .map_err(|e| InitError::InitAlephantKeys(e.to_string()))?;
            let mut map = HashMap::default();
            for vk in rows {
                map.insert(vk.key_hash.clone(), vk);
            }
            tracing::info!(count = map.len(), "loaded initial virtual_keys into cache");
            RwLock::new(Some(map))
        } else {
            RwLock::new(None)
        };

        let master_key_cache = match (&router_store, &master_key_encryption_key) {
            (Some(rs), Some(enc_key)) => {
                tracing::info!("master_key_cache initialised");
                Some(crate::store::master_key_cache::MasterKeyCache::new(
                    rs.pool.clone(),
                    enc_key.clone(),
                ))
            }
            _ => None,
        };

        // Capture providers before `config` is moved into InnerAppState.
        let initial_providers_config = config.providers.clone();

        let content_filter_initial = if config.policy.enabled {
            match crate::content_filter::ContentFilterGrpcClient::connect(
                config.policy.grpc_endpoint.clone(),
            )
            .await
            {
                Ok(client) => Some(Arc::new(client)),
                Err(e) => {
                    tracing::error!(
                        endpoint = %config.policy.grpc_endpoint,
                        error = %e,
                        "content_filter: initial policy gRPC connect failed; \
                         gateway will start and retry in background"
                    );
                    None
                }
            }
        } else {
            None
        };

        let content_filter =
            crate::content_filter::ContentFilterClientHolder::new(content_filter_initial);

        if config.policy.enabled {
            crate::content_filter::spawn_content_filter_reconnect_task(
                content_filter.reconnect_lock(),
                config.policy.grpc_endpoint.clone(),
                crate::config::policy::POLICY_GRPC_RECONNECT_INTERVAL,
            );
        }

        let redis = config
            .request_log
            .log_queue_redis_url
            .clone()
            .map(|url| Arc::new(crate::app_redis::AppRedis::new(url)));

        let request_log_transport = crate::logger::transport::build_request_log_transport(
            &config,
            &alephant_http_client,
            &metrics,
            redis.clone(),
        );

        let llm_kv = crate::llm_kv_cache::build_llm_kv_backend(&config).await?;
        if let Some(collection) = config.semantic_cache.qdrant.collection.as_deref() {
            tracing::warn!(
                collection = %collection,
                "semantic_cache.qdrant.collection is deprecated and ignored; collection names are derived at runtime"
            );
        }
        let semantic_cache = if config.semantic_cache.qdrant.url.trim().is_empty() {
            tracing::warn!(
                "semantic_cache.qdrant.url is empty; semantic cache \
                     disabled"
            );
            None
        } else {
            let openai_base_url_seed = initial_providers_config
                .get(&InferenceProvider::OpenAI)
                .map(|provider_cfg| provider_cfg.base_url.to_string());
            let base_url_resolver = EmbeddingBaseUrlResolver::from_runtime(
                openai_base_url_seed,
                redis.clone(),
                router_store.clone(),
            );
            let vector_store = Arc::new(QdrantStore {
                base_url: config.semantic_cache.qdrant.url.clone(),
                api_key: config.semantic_cache.qdrant.api_key.clone(),
                client: reqwest::Client::new(),
            });
            let embedder = Arc::new(OpenAiEmbedderClient::new(reqwest::Client::new()));
            Some(Arc::new(SemanticCacheService::new(
                embedder,
                vector_store,
                base_url_resolver,
                config.semantic_cache.default_threshold(),
                config.semantic_cache.default_ttl_seconds,
            )))
        };

        let app_state = AppState(Arc::new(InnerAppState {
            config,
            s3,
            router_store,
            alephant_http_client,
            request_log_transport,
            redis,
            provider_keys,
            metrics,
            endpoint_metrics,
            health_monitors: health_monitor,
            rate_limit_monitors: rate_limit_monitor,
            rate_limit_senders: RwLock::new(HashMap::default()),
            rate_limit_receivers: RwLock::new(HashMap::default()),
            router_tx: RwLock::new(None),
            alephant_api_keys,
            router_organization_map: RwLock::new(HashMap::default()),
            master_key_encryption_key,
            virtual_keys_cache,
            master_key_cache,
            providers_config: std::sync::RwLock::new(initial_providers_config),
            bare_model_expand_index: std::sync::RwLock::new(
                crate::discover::router::BareModelExpandIndex::default(),
            ),
            content_filter,
            workspace_provider_allowlist: std::sync::RwLock::new(rustc_hash::FxHashMap::default()),
            provider_is_router_flags: std::sync::RwLock::new(rustc_hash::FxHashMap::default()),
            llm_kv,
            semantic_cache,
            cache_warmed: AtomicBool::new(false),
        }));

        Ok(app_state)
    }

    /// Constructs the application's service stack, including all middleware
    /// layers and the main router.
    async fn build_service_stack(app_state: AppState) -> Result<BoxedServiceStack, InitError> {
        let meter = global::meter(SERVICE_NAME);
        let otel_metrics_layer = tower_otel_http_metrics::HTTPMetricsLayerBuilder::builder()
            .with_meter(meter)
            .with_response_extractor::<_, axum_core::body::Body>(AttributeExtractor)
            .build()?;

        let router = MetaRouter::build(app_state.clone()).await?;

        let client_ip_rate_limit_layer =
            crate::middleware::client_ip_rate_limit::ClientIpRateLimitLayer::new(&app_state)
                .await?;

        let gateway_in_flight_layer =
            crate::middleware::gateway_in_flight_limit::GatewayInFlightLimitLayer::new(&app_state)
                .await?;

        // Security config can be in global or unified_api section.
        // Global takes precedence; unified_api is the fallback.
        let security_config = app_state.config().global.security.as_ref().or(app_state
            .config()
            .unified_api
            .security
            .as_ref());

        let security_layer = match security_config {
            Some(config) => match config.to_loader_config() {
                Some(loader_config) => {
                    let loader = PluginLoader::from_config(&loader_config)
                        .map_err(|e| InitError::SecurityPluginConfig(e.to_string()))?;
                    let plugin_names = loader.plugin_names();
                    info!(plugins = ?plugin_names, "security plugins loaded");
                    SecurityLayer::new(loader)
                }
                None => SecurityLayer::disabled(),
            },
            None => SecurityLayer::disabled(),
        };

        let compression_layer = CompressionLayer::new()
            .gzip(true)
            .br(true)
            .deflate(true)
            .zstd(true);

        let cors_layer = CorsLayer::new()
            .allow_headers(Any)
            .allow_methods(Any)
            .allow_origin(Any);

        // global middleware is applied here
        let service_stack = ServiceBuilder::new()
            .layer(CatchPanicLayer::custom(PanicResponder))
            .layer(SetSensitiveHeadersLayer::new(std::iter::once(
                http::header::AUTHORIZATION,
            )))
            .layer(
                TraceLayer::new_for_http()
                    .make_span_with(SpanFactory::new(
                        Level::INFO,
                        app_state.config().telemetry.propagate,
                    ))
                    .on_body_chunk(())
                    .on_eos(()),
            )
            .layer(otel_metrics_layer)
            .set_x_request_id(MakeRequestId)
            .propagate_x_request_id()
            .layer(NormalizePathLayer::trim_trailing_slash())
            .layer(metrics::request_count::Layer::new(app_state.clone()))
            .layer(compression_layer)
            .layer(cors_layer)
            .layer(client_ip_rate_limit_layer)
            .layer(gateway_in_flight_layer)
            .layer(HealthCheckLayer::with_app_state(app_state.clone()))
            .layer(ValidateRouterConfigLayer::new())
            .layer(TimerLayer::new())
            .layer(ErrorHandlerLayer::new(app_state.clone()))
            .layer(ResponseHeaderLayer::new(
                app_state.response_headers_config(),
            ))
            .layer(security_layer)
            .map_err(crate::error::internal::InternalError::BufferError)
            .layer(BufferLayer::new(APP_BUFFER_SIZE))
            .layer(ErrorHandlerLayer::new(app_state.clone()))
            .service(router);

        Ok(BoxCloneService::new(service_stack))
    }
}

impl meltdown::Service for App {
    type Future = BoxFuture<'static, Result<(), RuntimeError>>;

    fn run(self, token: Token) -> Self::Future {
        Box::pin(async move {
            let app_state = self.state.clone();
            let config = app_state.config();
            let addr = SocketAddr::from((config.server.address, config.server.port));
            info!(address = %addr, tls = %config.server.tls, "server starting");

            let handle = axum_server::Handle::new();
            let app_factory = AppFactory::new_hyper_app(self);
            // sleep so that the banner is not printed before the server is
            // ready
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            cli::helpers::show_welcome_banner(&addr);

            match &config.server.tls {
                TlsConfig::Enabled { cert, key } => {
                    let tls_config = RustlsConfig::from_pem_file(cert.clone(), key.clone())
                        .await
                        .map_err(InitError::Tls)?;

                    tokio::select! {
                        biased;
                        server_output = axum_server::bind_rustls(addr, tls_config)
                            // Why `NoDelayAcceptor`? See:
                            // https://brooker.co.za/blog/2024/05/09/nagle.html
                            .acceptor(NoDelayAcceptor)
                            .handle(handle.clone())
                            .serve(app_factory) => server_output.map_err(RuntimeError::Serve)?,
                        () = token => {
                            handle.graceful_shutdown(Some(config.server.shutdown_timeout));
                        }
                    };
                }
                TlsConfig::Disabled => {
                    tokio::select! {
                        biased;
                        server_output = axum_server::bind(addr)
                            .handle(handle.clone())
                            .serve(app_factory) => server_output.map_err(RuntimeError::Serve)?,
                        () = token => {
                            handle.graceful_shutdown(Some(config.server.shutdown_timeout));
                        }
                    };
                }
            }
            Ok(())
        })
    }
}

#[derive(Clone)]
pub struct HyperApp {
    pub state: AppState,
    pub service_stack: BoxedHyperServiceStack,
}

impl HyperApp {
    #[must_use]
    pub fn new(app: App) -> Self {
        let state = app.state.clone();
        let service_stack = ServiceBuilder::new()
            .map_request(|req: http::Request<hyper::body::Incoming>| {
                req.map(axum_core::body::Body::new)
            })
            .service(app);
        Self {
            state,
            service_stack: BoxCloneService::new(service_stack),
        }
    }
}

impl tower::Service<http::Request<hyper::body::Incoming>> for HyperApp {
    type Response = AppResponse;
    type Error = Infallible;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    #[inline]
    fn poll_ready(&mut self, ctx: &mut Context<'_>) -> std::task::Poll<Result<(), Self::Error>> {
        self.service_stack.poll_ready(ctx)
    }

    #[inline]
    fn call(&mut self, req: http::Request<hyper::body::Incoming>) -> Self::Future {
        self.service_stack.call(req)
    }
}

#[derive(Clone)]
pub struct AppFactory<S> {
    pub state: AppState,
    pub inner: S,
}

impl<S> AppFactory<S> {
    pub fn new(state: AppState, inner: S) -> Self {
        Self { state, inner }
    }
}

impl AppFactory<HyperApp> {
    #[must_use]
    pub fn new_hyper_app(app: App) -> Self {
        Self {
            state: app.state.clone(),
            inner: HyperApp::new(app),
        }
    }
}

impl<S> tower::Service<SocketAddr> for AppFactory<S>
where
    S: Clone,
{
    type Response = AddExtension<S, SocketAddr>;
    type Error = Infallible;
    type Future = Ready<Result<Self::Response, Self::Error>>;

    #[inline]
    fn poll_ready(&mut self, _ctx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, socket: SocketAddr) -> Self::Future {
        // see: https://docs.rs/tower/latest/tower/trait.Service.html#be-careful-when-cloning-inner-services
        let mut inner = self.inner.clone();
        std::mem::swap(&mut self.inner, &mut inner);
        let svc = ServiceBuilder::new()
            .layer(tower_http::add_extension::AddExtensionLayer::new(socket))
            .service(inner);
        ready(Ok(svc))
    }
}

/// Lightweight `App` constructor for tests that don't need a database.
///
/// Skips PG connection, `router_store`, `virtual_keys_cache`,
/// `master_key_cache`, and `master_key_encryption_key`.
#[cfg(any(test, feature = "testing"))]
#[allow(clippy::unused_async)]
#[cfg_attr(feature = "internal", allow(unused_mut))]
pub async fn build_test_app(mut config: Config) -> Result<App, InitError> {
    #[cfg(feature = "external")]
    if config.cloudflare_kv.is_none() {
        use crate::{config::cloudflare_kv::CloudflareKvConfig, types::secret::Secret};
        config.cloudflare_kv = Some(CloudflareKvConfig {
            api_base: "https://api.cloudflare.com/client/v4".into(),
            account_id: "test".into(),
            namespace_id: "test".into(),
            api_token: Secret::from("test-token".to_string()),
        });
    }

    let llm_kv = crate::llm_kv_cache::build_llm_kv_backend(&config).await?;

    let s3 = BaseS3Client::new(config.s3.clone())?;
    let alephant_http_client = AlephantHttpClient::new()?;
    let meter = opentelemetry::global::meter(SERVICE_NAME);
    let metrics = metrics::Metrics::new(&meter);
    let endpoint_metrics = EndpointMetricsRegistry::new(&config);

    let provider_keys = ProviderKeys::new(&config, &metrics);
    let initial_providers_config = config.providers.clone();

    let redis = config
        .request_log
        .log_queue_redis_url
        .clone()
        .map(|url| Arc::new(crate::app_redis::AppRedis::new(url)));

    let request_log_transport = crate::logger::transport::build_request_log_transport(
        &config,
        &alephant_http_client,
        &metrics,
        redis.clone(),
    );

    let app_state = AppState(Arc::new(InnerAppState {
        config,
        s3,
        router_store: None,
        alephant_http_client,
        request_log_transport,
        redis,
        provider_keys,
        metrics,
        endpoint_metrics,
        health_monitors: HealthMonitorMap::default(),
        rate_limit_monitors: RateLimitMonitorMap::default(),
        rate_limit_senders: tokio::sync::RwLock::new(HashMap::default()),
        rate_limit_receivers: tokio::sync::RwLock::new(HashMap::default()),
        router_tx: tokio::sync::RwLock::new(None),
        alephant_api_keys: tokio::sync::RwLock::new(None),
        router_organization_map: tokio::sync::RwLock::new(HashMap::default()),
        master_key_encryption_key: None,
        virtual_keys_cache: tokio::sync::RwLock::new(None),
        master_key_cache: None,
        providers_config: std::sync::RwLock::new(initial_providers_config),
        bare_model_expand_index: std::sync::RwLock::new(
            crate::discover::router::BareModelExpandIndex::default(),
        ),
        content_filter: crate::content_filter::ContentFilterClientHolder::new(None),
        workspace_provider_allowlist: std::sync::RwLock::new(rustc_hash::FxHashMap::default()),
        provider_is_router_flags: std::sync::RwLock::new(rustc_hash::FxHashMap::default()),
        llm_kv,
        semantic_cache: None,
        cache_warmed: AtomicBool::new(false),
    }));

    Ok(App {
        state: app_state.clone(),
        service_stack: tower::util::BoxCloneService::new(tower::service_fn(
            |_req: crate::types::request::Request| async {
                Ok::<AppResponse, std::convert::Infallible>(
                    http::Response::builder()
                        .status(503)
                        .body(AppResponseBody::default())
                        .unwrap(),
                )
            },
        )),
    })
}
