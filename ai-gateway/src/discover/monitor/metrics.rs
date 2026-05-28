use std::{
    sync::{Arc, OnceLock, RwLock},
    time::Duration,
};

use rustc_hash::FxHashMap as HashMap;

use crate::{
    config::Config, endpoints::ApiEndpoint, error::internal::InternalError,
    metrics::RollingCounter, types::provider::InferenceProvider,
};

static CUSTOM_ENDPOINT_METRICS_DISCARD: OnceLock<Arc<EndpointMetrics>> = OnceLock::new();

#[inline]
fn custom_endpoint_metrics_discard() -> Arc<EndpointMetrics> {
    Arc::clone(CUSTOM_ENDPOINT_METRICS_DISCARD.get_or_init(|| Arc::new(EndpointMetrics::default())))
}

/// We use this to track metrics for monitoring provider health.
///
/// We do this separately from the OpenTelemetry metrics because a) they
/// don't provide a way to query the metrics and b) it's easy to implement
/// the rolling window this way.
#[derive(Debug, Clone)]
pub struct EndpointMetricsRegistry {
    endpoint_health_metrics: Arc<RwLock<HashMap<ApiEndpoint, Arc<EndpointMetrics>>>>,
}

impl EndpointMetricsRegistry {
    pub fn health_metrics(
        &self,
        api_endpoint: ApiEndpoint,
    ) -> Result<Arc<EndpointMetrics>, InternalError> {
        if matches!(
            &api_endpoint,
            ApiEndpoint::OpenAICompatible {
                provider: InferenceProvider::Custom,
                ..
            }
        ) {
            return Ok(custom_endpoint_metrics_discard());
        }

        if let Some(metrics) = self
            .endpoint_health_metrics
            .read()
            .expect("endpoint health metrics lock poisoned")
            .get(&api_endpoint)
        {
            return Ok(Arc::clone(metrics));
        }

        if matches!(
            &api_endpoint,
            ApiEndpoint::OpenAICompatible {
                provider: InferenceProvider::Named(_),
                ..
            }
        ) {
            let mut guard = self
                .endpoint_health_metrics
                .write()
                .expect("endpoint health metrics lock poisoned");
            let metrics = guard
                .entry(api_endpoint)
                .or_insert_with(|| Arc::new(EndpointMetrics::default()));
            return Ok(Arc::clone(metrics));
        }

        Err(InternalError::MetricsNotConfigured(api_endpoint))
    }

    pub fn new(config: &Config) -> Self {
        let mut endpoint_health_metrics = HashMap::default();
        tracing::debug!(
            providers = ?config.providers.keys(),
            "Initializing endpoint metrics for providers"
        );
        for provider in config.providers.keys() {
            tracing::trace!(
                provider = ?provider,
                endpoints = ?provider.endpoints(),
                "Initializing endpoint metrics for provider"
            );
            for endpoint in provider.endpoints() {
                endpoint_health_metrics.insert(endpoint, Arc::new(EndpointMetrics::default()));
            }
        }
        Self {
            endpoint_health_metrics: Arc::new(RwLock::new(endpoint_health_metrics)),
        }
    }
}

#[derive(Debug, Default)]
pub struct EndpointMetrics {
    /// total request count
    pub(crate) request_count: RollingCounter,
    /// Count of upstream remote internal errors
    pub(crate) remote_internal_error_count: RollingCounter,
}

impl EndpointMetrics {
    #[must_use]
    pub fn new(window: Duration, buckets: u32) -> Self {
        Self {
            request_count: RollingCounter::new(window, buckets),
            remote_internal_error_count: RollingCounter::new(window, buckets),
        }
    }

    pub fn incr_req_count(&self) {
        self.request_count.incr();
    }

    pub fn incr_remote_internal_error_count(&self) {
        self.remote_internal_error_count.incr();
    }

    pub fn incr_for_stream_error(&self, stream_error: &reqwest_eventsource::Error) {
        match stream_error {
            reqwest_eventsource::Error::StreamEnded => {
                // happens in valid stream end cases, so we dont
                // increment metrics heres
            }
            reqwest_eventsource::Error::InvalidStatusCode(status_code, ..) => {
                if status_code.is_server_error() {
                    tracing::error!(status_code = %status_code, "got upstream server error in stream");
                    self.incr_remote_internal_error_count();
                } else if status_code.is_client_error() {
                    tracing::debug!(status_code = %status_code, "got upstream client error in stream");
                }
            }
            reqwest_eventsource::Error::Utf8(..)
            | reqwest_eventsource::Error::Parser(..)
            | reqwest_eventsource::Error::Transport(..)
            | reqwest_eventsource::Error::InvalidContentType(..)
            | reqwest_eventsource::Error::InvalidLastEventId(..) => {
                tracing::error!(
                    error = %stream_error,
                    "encountered invalid stream error"
                );
                // we want to count these as errors in our health metrics
                // so that if someone returns garbled utf88 for example,
                // we still consider that a health issue and can remove
                // them from the lb pool
                self.incr_remote_internal_error_count();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{Config, providers::ProvidersConfig},
        endpoints::{ApiEndpoint, openai::OpenAI},
    };

    #[test]
    fn health_metrics_openai_compatible_custom_ok_when_registry_empty() {
        let providers: ProvidersConfig = serde_yml::from_str("{}").expect("empty providers map");
        let config = Config {
            providers,
            ..Default::default()
        };
        let registry = EndpointMetricsRegistry::new(&config);
        let endpoint = ApiEndpoint::OpenAICompatible {
            provider: InferenceProvider::Custom,
            openai_endpoint: OpenAI::chat_completions(),
        };
        assert!(registry.health_metrics(endpoint).is_ok());
    }

    #[test]
    fn health_metrics_openai_ok_with_default_embedded_providers() {
        let registry = EndpointMetricsRegistry::new(&Config::default());
        let endpoint = ApiEndpoint::OpenAI(OpenAI::chat_completions());
        assert!(registry.health_metrics(endpoint).is_ok());
    }

    #[test]
    fn health_metrics_openai_compatible_named_ok_when_provider_is_db_only() {
        let providers: ProvidersConfig = serde_yml::from_str("{}").expect("empty providers map");
        let config = Config {
            providers,
            ..Default::default()
        };
        let registry = EndpointMetricsRegistry::new(&config);
        let endpoint = ApiEndpoint::OpenAICompatible {
            provider: InferenceProvider::Named("qwen-beijing".into()),
            openai_endpoint: OpenAI::chat_completions(),
        };

        assert!(registry.health_metrics(endpoint).is_ok());
    }
}
