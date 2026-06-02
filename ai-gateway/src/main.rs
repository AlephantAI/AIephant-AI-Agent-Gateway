use std::path::PathBuf;

use ai_gateway::{
    app::App,
    config::{Config, fallback_bridge},
    discover::monitor::{
        health::provider::HealthMonitor, rate_limit::RateLimitMonitor,
    },
    error::{init::InitError, runtime::RuntimeError},
    metrics::system::SystemMetrics,
    store::db_listener::DatabaseListener,
    utils::meltdown::TaggedService,
};
use clap::Parser;
use meltdown::Meltdown;
use opentelemetry_sdk::{
    logs::SdkLoggerProvider, metrics::SdkMeterProvider,
    trace::SdkTracerProvider,
};
use tracing::{debug, info};

#[global_allocator]
static GLOBAL: jemallocator::Jemalloc = jemallocator::Jemalloc;

#[derive(Debug, Parser)]
#[command(version)]
pub struct Args {
    /// Path to the default config file.
    /// Configs in this file can be overridden by environment variables.
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Enable verbose logging
    #[arg(short, long)]
    verbose: bool,
}

#[tokio::main]
async fn main() -> Result<(), RuntimeError> {
    // Install the crypto provider before any TLS operations
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");
    let config = load_and_validate_config()?;
    let (logger_provider, tracer_provider, metrics_provider) =
        init_telemetry(&config)?;

    run_app(config).await?;

    shutdown_telemetry(logger_provider, &tracer_provider, metrics_provider);

    info!("shut down");

    Ok(())
}

fn load_and_validate_config() -> Result<Config, RuntimeError> {
    dotenvy::dotenv().ok();
    let args = Args::parse();
    let mut config = match Config::try_read(args.config.as_ref()) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("failed to read config: {error}");
            std::process::exit(1);
        }
    };

    // Override telemetry level if verbose flag is provided
    if args.verbose {
        config.telemetry.level = "info,ai_gateway=trace".to_string();
    }

    config.validate().inspect_err(|e| {
        tracing::error!(error = %e, "configuration validation failed");
    })?;
    // Emit one-time deprecation warnings for per-path retry configs that are
    // superseded by fallback-policy.retry.
    fallback_bridge::warn_deprecated_per_path_retries(&config);

    Ok(config)
}

fn init_telemetry(
    config: &Config,
) -> Result<
    (
        Option<SdkLoggerProvider>,
        SdkTracerProvider,
        Option<SdkMeterProvider>,
    ),
    InitError,
> {
    let (logger_provider, tracer_provider, metrics_provider) =
        telemetry::init_telemetry(&config.telemetry)?;

    debug!("telemetry initialized");
    let pretty_config = serde_yml::to_string(&config)
        .expect("config should always be serializable");
    tracing::debug!(config = pretty_config, "Creating app with config");

    #[cfg(debug_assertions)]
    tracing::warn!("running in debug mode");

    Ok((logger_provider, tracer_provider, metrics_provider))
}

async fn run_app(config: Config) -> Result<(), RuntimeError> {
    let mut shutting_down = false;
    let app = App::new(config).await?;
    let app_state = app.state.clone();
    let config = app_state.config();
    let health_monitor = HealthMonitor::new(app_state.clone());
    let rate_limit_monitor = RateLimitMonitor::new(app_state.clone());

    let mut tasks = vec![
        "shutdown-signals",
        "gateway",
        "provider-health-monitor",
        "provider-rate-limit-monitor",
        "system-metrics",
    ];
    let mut meltdown = Meltdown::new().register(TaggedService::new(
        "shutdown-signals",
        ai_gateway::utils::meltdown::wait_for_shutdown_signals,
    ));

    if !config.compat_mode {
        meltdown = meltdown.register(TaggedService::new(
            "database-listener",
            DatabaseListener::new(
                config.database.url.expose(),
                app_state.clone(),
            )
            .await?,
        ));
        tasks.push("database-listener");
    }

    meltdown = meltdown
        .register(TaggedService::new("gateway", app))
        .register(TaggedService::new(
            "provider-health-monitor",
            health_monitor,
        ))
        .register(TaggedService::new(
            "provider-rate-limit-monitor",
            rate_limit_monitor,
        ))
        .register(TaggedService::new("system-metrics", SystemMetrics));

    info!(tasks = ?tasks, "starting services");

    while let Some((service, result)) = meltdown.next().await {
        match result {
            Ok(()) => info!(%service, "service stopped successfully"),
            Err(error) => tracing::error!(%service, %error, "service crashed"),
        }

        if !shutting_down {
            info!("propagating shutdown signal...");
            meltdown.trigger();
            shutting_down = true;
        }
    }
    Ok(())
}

fn shutdown_telemetry(
    logger_provider: Option<SdkLoggerProvider>,
    tracer_provider: &SdkTracerProvider,
    metrics_provider: Option<SdkMeterProvider>,
) {
    if let Some(logger_provider) = logger_provider
        && let Err(e) = logger_provider.shutdown()
    {
        info!("error shutting down logger provider: {e}");
    }
    if let Err(e) = tracer_provider.shutdown() {
        info!("error shutting down tracer provider: {e}");
    }
    if let Some(metrics_provider) = metrics_provider
        && let Err(e) = metrics_provider.shutdown()
    {
        info!("error shutting down metrics provider: {e}");
    }
}
