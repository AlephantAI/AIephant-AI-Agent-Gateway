use std::sync::Arc;

use rustc_hash::FxHashMap as HashMap;
use tower::ServiceBuilder;

use crate::{
    app_state::AppState,
    dispatcher::{Dispatcher, DispatcherService},
    error::init::InitError,
    middleware::request_context,
    types::provider::InferenceProvider,
};

pub type DirectProxyService = request_context::Service<DispatcherService>;

#[derive(Debug, Clone)]
pub struct DirectProxies(Arc<HashMap<InferenceProvider, DirectProxyService>>);

impl DirectProxies {
    pub async fn new(app_state: &AppState) -> Result<Self, InitError> {
        let mut direct_proxies = HashMap::default();
        for (provider, _provider_config) in app_state.get_providers_config().iter() {
            let direct_proxy_dispatcher =
                Dispatcher::new_direct_proxy(app_state.clone(), provider).await?;

            let direct_proxy = ServiceBuilder::new()
                .layer(request_context::Layer::for_direct_proxy())
                .service(direct_proxy_dispatcher);

            direct_proxies.insert(provider.clone(), direct_proxy);
        }
        Ok(Self(Arc::new(direct_proxies)))
    }
}

impl std::ops::Deref for DirectProxies {
    type Target = Arc<HashMap<InferenceProvider, DirectProxyService>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
