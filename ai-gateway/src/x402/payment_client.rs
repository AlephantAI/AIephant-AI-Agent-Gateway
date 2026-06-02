use std::sync::Arc;

use tokio::sync::RwLock;
use tonic::transport::Channel;

use crate::{
    error::init::InitError,
    payment_proto::x402_payment_service_client::X402PaymentServiceClient,
};

#[derive(Clone, Debug)]
pub struct X402PaymentGrpcClient {
    inner: X402PaymentServiceClient<Channel>,
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

impl X402PaymentGrpcClient {
    pub async fn connect(endpoint: String) -> Result<Self, InitError> {
        const MAX: usize = 4 * 1024 * 1024;
        let uri = normalize_grpc_endpoint(&endpoint);
        let ch = tonic::transport::Endpoint::from_shared(uri)
            .map_err(|e| InitError::PolicyGrpcConnect(e.to_string()))?
            .connect()
            .await
            .map_err(|e| InitError::PolicyGrpcConnect(e.to_string()))?;
        let inner = X402PaymentServiceClient::new(ch)
            .max_decoding_message_size(MAX)
            .max_encoding_message_size(MAX);
        Ok(Self { inner })
    }

    #[must_use]
    pub fn inner(&self) -> X402PaymentServiceClient<Channel> {
        self.inner.clone()
    }
}

#[derive(Clone)]
pub struct X402PaymentClientHolder {
    inner: Arc<RwLock<Option<Arc<X402PaymentGrpcClient>>>>,
}

impl std::fmt::Debug for X402PaymentClientHolder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("X402PaymentClientHolder")
    }
}

impl X402PaymentClientHolder {
    #[must_use]
    pub fn new(initial: Option<Arc<X402PaymentGrpcClient>>) -> Self {
        Self {
            inner: Arc::new(RwLock::new(initial)),
        }
    }

    pub async fn get(&self) -> Option<Arc<X402PaymentGrpcClient>> {
        self.inner.read().await.clone()
    }
}
