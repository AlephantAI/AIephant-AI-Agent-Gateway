use std::{
    convert::Infallible,
    future::poll_fn,
    net::{IpAddr, Ipv4Addr, SocketAddr},
};

use futures::future::BoxFuture;
use tower::MakeService as _;
use url::Url;

use super::mock::{Mock, MockArgs};
use crate::{
    app::{App, AppFactory, AppResponse},
    config::Config,
    types::request::Request,
};

pub const MOCK_SERVER_PORT: u16 = 8111;

#[derive(Default)]
pub struct HarnessBuilder {
    mock_args: Option<MockArgs>,
    config: Option<Config>,
    /// Override alephant `log_collector_url` after mock setup (e.g. for log
    /// capture server).
    log_collector_url_override: Option<Url>,
}

impl HarnessBuilder {
    #[must_use]
    pub fn with_mock_args(mut self, mock_args: MockArgs) -> Self {
        self.mock_args = Some(mock_args);
        self
    }

    #[must_use]
    pub fn with_config(mut self, config: Config) -> Self {
        self.config = Some(config);
        self
    }

    pub async fn build(self) -> Harness {
        let config = self.config.expect("config is required");
        let mock_args = self
            .mock_args
            .unwrap_or_else(|| MockArgs::builder().build());
        let log_collector_url_override = self.log_collector_url_override;
        Harness::new(mock_args, config, log_collector_url_override).await
    }

    /// No-op in Cloud-only mode — kept for API compatibility with existing
    /// integration tests that will be updated in a subsequent task.
    #[must_use]
    pub fn with_mock_auth(self) -> Self {
        self
    }

    /// No-op in Cloud-only mode.
    #[must_use]
    pub fn with_auth_keys(
        self,
        _keys: Vec<crate::virtual_key::legacy_key::Key>,
    ) -> Self {
        self
    }

    /// Override `config.alephant.log_collector_url` after mock setup (e.g. to
    /// point to a log capture server).
    #[must_use]
    pub fn with_log_collector_url(mut self, url: Url) -> Self {
        self.log_collector_url_override = Some(url);
        self
    }
}
pub struct Harness {
    pub app_factory: AppFactory<App>,
    pub mock: Mock,
    pub socket_addr: SocketAddr,
}

impl Harness {
    async fn new(
        mock_args: MockArgs,
        mut config: Config,
        log_collector_url_override: Option<Url>,
    ) -> Self {
        let mock = Mock::new(&mut config, mock_args).await;
        if let Some(url) = log_collector_url_override {
            config.alephant.log_collector_url = url;
        }
        let app = App::new(config).await.expect("failed to create app");
        let app_factory = AppFactory::new(app.state.clone(), app);
        let socket_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
        Self {
            app_factory,
            mock,
            socket_addr,
        }
    }

    #[must_use]
    pub fn builder() -> HarnessBuilder {
        HarnessBuilder::default()
    }

    pub async fn shutdown(self) {
        if let Some(router_store) = self.app_factory.state.router_store() {
            router_store.pool.close().await;
        }
    }
}

impl tower::Service<Request> for Harness {
    type Response = AppResponse;
    type Error = Infallible;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        tower::MakeService::poll_ready(&mut self.app_factory, cx)
    }

    fn call(&mut self, req: Request) -> Self::Future {
        let mut factory = self.app_factory.clone();
        let socket_addr = self.socket_addr;
        std::mem::swap(&mut self.app_factory, &mut factory);
        Box::pin(async move {
            let mut app =
                factory.into_service().call(socket_addr).await.unwrap();
            poll_fn(|cx| tower::Service::poll_ready(&mut app, cx))
                .await
                .unwrap();

            app.call(req).await
        })
    }
}
