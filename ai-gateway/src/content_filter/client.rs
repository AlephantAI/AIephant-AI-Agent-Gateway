use std::sync::Arc;

use tokio::sync::RwLock;
use tonic::transport::Channel;

use crate::{error::init::InitError, policy_proto::policy_service_client::PolicyServiceClient};

#[derive(Clone, Debug)]
pub struct ContentFilterGrpcClient {
    inner: PolicyServiceClient<Channel>,
}

fn normalize_grpc_endpoint(endpoint: &str) -> String {
    let ep = endpoint.trim();
    if ep.is_empty() {
        return String::new();
    }
    if ep.starts_with("http://") || ep.starts_with("https://") {
        ep.to_string()
    } else {
        format!("http://{ep}")
    }
}

impl ContentFilterGrpcClient {
    pub async fn connect(endpoint: String) -> Result<Self, InitError> {
        // Large enough for `EvaluateRequest.body` plus
        // `EvaluateResponse.out_body` (each bounded by policy config,
        // typically ≤ 1 MiB per direction).
        const MAX: usize = 4 * 1024 * 1024;
        let uri = normalize_grpc_endpoint(&endpoint);
        let ch = tonic::transport::Endpoint::from_shared(uri)
            .map_err(|e| InitError::PolicyGrpcConnect(e.to_string()))?
            .connect()
            .await
            .map_err(|e| InitError::PolicyGrpcConnect(e.to_string()))?;
        let inner = PolicyServiceClient::new(ch)
            .max_decoding_message_size(MAX)
            .max_encoding_message_size(MAX);
        Ok(Self { inner })
    }

    #[must_use]
    pub fn inner(&self) -> PolicyServiceClient<Channel> {
        self.inner.clone()
    }
}

/// Shared slot for the policy gRPC client, filled on startup or by the
/// reconnect task.
#[derive(Clone)]
pub struct ContentFilterClientHolder {
    inner: Arc<RwLock<Option<Arc<ContentFilterGrpcClient>>>>,
}

impl std::fmt::Debug for ContentFilterClientHolder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ContentFilterClientHolder")
    }
}

impl ContentFilterClientHolder {
    #[must_use]
    pub fn new(initial: Option<Arc<ContentFilterGrpcClient>>) -> Self {
        Self {
            inner: Arc::new(RwLock::new(initial)),
        }
    }

    pub async fn get(&self) -> Option<Arc<ContentFilterGrpcClient>> {
        self.inner.read().await.clone()
    }

    pub(crate) fn reconnect_lock(&self) -> Arc<RwLock<Option<Arc<ContentFilterGrpcClient>>>> {
        self.inner.clone()
    }
}
