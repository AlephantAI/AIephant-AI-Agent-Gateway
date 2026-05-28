use http::Extensions;
use typed_builder::TypedBuilder;

use crate::types::{
    extensions::{AuthContext, MapperContext, MapperProfileContext, ProviderRequestId},
    provider::InferenceProvider,
    router::RouterId,
};

#[derive(Debug, TypedBuilder)]
pub struct ExtensionsCopier {
    inference_provider: InferenceProvider,
    router_id: Option<RouterId>,
    auth_context: Option<AuthContext>,
    provider_request_id: Option<http::HeaderValue>,
    mapper_ctx: MapperContext,
    mapper_profile_context: Option<MapperProfileContext>,
}

impl ExtensionsCopier {
    /// Copies required request extensions to response extensions.
    pub fn copy_extensions(self, resp_extensions: &mut Extensions) {
        // MapperContext, ApiEndpoint, and PathAndQuery are copied out of bound
        // from this helper because we already removed them from the request
        // extensions in order to use them in the dispatcher service logic.
        resp_extensions.insert(self.inference_provider);
        if let Some(router_id) = self.router_id {
            resp_extensions.insert(router_id);
        }
        if let Some(auth_context) = self.auth_context {
            resp_extensions.insert(auth_context);
        }
        if let Some(provider_request_id) = self.provider_request_id {
            resp_extensions.insert(ProviderRequestId(provider_request_id));
        }
        resp_extensions.insert(self.mapper_ctx);
        if let Some(mapper_profile_context) = self.mapper_profile_context {
            resp_extensions.insert(mapper_profile_context);
        }
    }
}

#[cfg(test)]
mod tests {
    use http::Extensions;

    use crate::{
        dispatcher::extensions::ExtensionsCopier,
        middleware::mapper::non_stream_profile_data::default_non_stream_profile,
        types::{
            extensions::{MapperContext, MapperProfileContext},
            provider::InferenceProvider,
        },
    };

    #[test]
    fn extensions_copier_copies_mapper_profile_context() {
        let mapper_profile_context = MapperProfileContext {
            provider: InferenceProvider::Named("deepseek".into()),
            raw_model: "deepseek/deepseek-reasoner".into(),
            non_stream_profile: default_non_stream_profile(&InferenceProvider::Named(
                "deepseek".into(),
            )),
        };
        let copier = ExtensionsCopier::builder()
            .inference_provider(InferenceProvider::Named("deepseek".into()))
            .router_id(None)
            .auth_context(None)
            .provider_request_id(None)
            .mapper_ctx(MapperContext {
                is_stream: false,
                model: None,
                anthropic_openai_usage: None,
                unified_responses_bridge_chat_completions_sse: false,
                native_semantic_passthrough: false,
                cursor_responses_via_chat_completions: false,
                cursor_responses_origin: None,
                client_expects_responses_wire: false,
            })
            .mapper_profile_context(Some(mapper_profile_context.clone()))
            .build();
        let mut resp_extensions = Extensions::new();

        copier.copy_extensions(&mut resp_extensions);

        let copied = resp_extensions
            .get::<MapperProfileContext>()
            .expect("mapper profile context should be copied");
        assert_eq!(copied.provider, mapper_profile_context.provider);
        assert_eq!(copied.raw_model, mapper_profile_context.raw_model);
    }
}
