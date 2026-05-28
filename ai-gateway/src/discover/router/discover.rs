use std::{
    convert::Infallible,
    pin::Pin,
    task::{Context, Poll},
};

use futures::Stream;
use pin_project_lite::pin_project;
use tokio::sync::mpsc::Receiver;
use tower::discover::Change;

use crate::{
    app_state::AppState, discover::router::provider_db::ProviderDbDiscovery,
    error::init::InitError, router::service::Router, types::router::RouterId,
};

pin_project! {
    /// Discovers routers from the `providers` / `provider_models` DB tables
    /// via [`ProviderDbDiscovery`]; hot-updates arrive on the `rx` channel
    /// provided by `db_listener`.
    #[derive(Debug)]
    pub struct RouterDiscovery {
        #[pin]
        inner: ProviderDbDiscovery,
    }
}

impl RouterDiscovery {
    pub async fn new(
        app_state: &AppState,
        rx: Receiver<Change<RouterId, Router>>,
    ) -> Result<Self, InitError> {
        Ok(Self {
            inner: ProviderDbDiscovery::new(app_state, rx).await?,
        })
    }

    #[must_use]
    pub async fn without_database(
        app_state: &AppState,
        rx: Receiver<Change<RouterId, Router>>,
    ) -> Result<Self, InitError> {
        Ok(Self {
            inner: ProviderDbDiscovery::without_database(app_state, rx).await?,
        })
    }
}

impl Stream for RouterDiscovery {
    type Item = Result<Change<RouterId, Router>, Infallible>;

    fn poll_next(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.project()
            .inner
            .poll_next(ctx)
            .map(|p| p.map(Result::Ok))
    }
}
