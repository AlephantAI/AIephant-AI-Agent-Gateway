use std::sync::Arc;

use rustc_hash::FxHashMap as HashMap;

use super::{
    EndpointConverter, TypedEndpointConverter, anthropic::AnthropicConverter,
    capabilities::ProviderCapabilities, google::GoogleConverter,
    model::ModelMapper, openai::OpenAIConverter,
    openai_compatible::OpenAICompatibleConverter,
    profile_resolver::resolve_mapper_metadata,
    rule_data::default_provider_rules, rule_validator::validate_provider_rules,
    rules::ProviderRuleSet,
};
use crate::{
    endpoints::{
        self, ApiEndpoint, anthropic::Anthropic, bedrock::Bedrock,
        google::Google, ollama::Ollama, openai::OpenAI,
    },
    middleware::mapper::{bedrock::BedrockConverter, ollama::OllamaConverter},
    types::provider::InferenceProvider,
};

#[derive(Debug, Default, Clone)]
pub struct EndpointConverterRegistry(Arc<EndpointConverterRegistryInner>);

impl EndpointConverterRegistry {
    #[must_use]
    pub fn new(model_mapper: &ModelMapper) -> Self {
        let inner = EndpointConverterRegistryInner::new(model_mapper);
        Self(Arc::new(inner))
    }

    #[must_use]
    pub fn get_converter(
        &self,
        source_endpoint: &ApiEndpoint,
        target_endpoint: &ApiEndpoint,
    ) -> Option<&(dyn EndpointConverter + Send + Sync + 'static)> {
        self.0
            .converters
            .get(&RegistryKey::new(
                source_endpoint.clone(),
                target_endpoint.clone(),
            ))
            .map(|v| &**v)
    }

    #[must_use]
    pub fn get_provider_capabilities(
        &self,
        provider: &InferenceProvider,
    ) -> Option<&ProviderCapabilities> {
        self.0.capabilities.get(provider)
    }

    #[must_use]
    pub fn get_provider_rules(
        &self,
        provider: &InferenceProvider,
    ) -> Option<&ProviderRuleSet> {
        self.0.rules.get(provider)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RegistryKey {
    source_endpoint: ApiEndpoint,
    target_endpoint: ApiEndpoint,
}

impl RegistryKey {
    fn new(source_endpoint: ApiEndpoint, target_endpoint: ApiEndpoint) -> Self {
        Self {
            source_endpoint,
            target_endpoint,
        }
    }
}

#[derive(Default)]
struct EndpointConverterRegistryInner {
    /// In the future when we support other APIs beside just chat completion
    /// we'll want to add another level here.
    converters: HashMap<
        RegistryKey,
        Box<dyn EndpointConverter + Send + Sync + 'static>,
    >,
    capabilities: HashMap<InferenceProvider, ProviderCapabilities>,
    rules: HashMap<InferenceProvider, ProviderRuleSet>,
}

impl std::fmt::Debug for EndpointConverterRegistryInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut debug = f.debug_struct("EndpointConverterRegistryInner");
        debug.field("converters", &self.converters.keys().collect::<Vec<_>>());
        debug.finish()
    }
}

impl EndpointConverterRegistryInner {
    #[allow(clippy::too_many_lines)]
    fn new(model_mapper: &ModelMapper) -> Self {
        let mut registry = Self {
            converters: HashMap::default(),
            capabilities: HashMap::default(),
            rules: HashMap::default(),
        };

        registry.register_provider_metadata(InferenceProvider::OpenAI);
        registry.register_provider_metadata(InferenceProvider::Anthropic);
        registry.register_provider_metadata(InferenceProvider::GoogleGemini);
        registry.register_provider_metadata(InferenceProvider::Ollama);
        registry.register_provider_metadata(InferenceProvider::Bedrock);
        registry.register_named_openai_compatible_providers(model_mapper);
        registry.register_provider_metadata(InferenceProvider::Custom);
        registry.register_openai_compatible_converter(
            InferenceProvider::Custom,
            model_mapper,
        );

        let key = RegistryKey::new(
            ApiEndpoint::OpenAI(OpenAI::chat_completions()),
            ApiEndpoint::Anthropic(Anthropic::messages()),
        );
        let anthropic_provider = InferenceProvider::Anthropic;
        let anthropic_capabilities = registry
            .capabilities
            .get(&anthropic_provider)
            .cloned()
            .expect("anthropic capabilities must exist");
        let anthropic_rules = registry
            .rules
            .get(&anthropic_provider)
            .cloned()
            .expect("anthropic rules must exist");
        let converter = TypedEndpointConverter::<
            endpoints::openai::ChatCompletions,
            endpoints::anthropic::Messages,
            AnthropicConverter,
        >::new(AnthropicConverter::new_with_metadata(
            anthropic_capabilities,
            anthropic_rules,
            model_mapper.clone(),
        ));
        registry.register_converter(key, converter);

        let key = RegistryKey::new(
            ApiEndpoint::OpenAI(OpenAI::chat_completions()),
            ApiEndpoint::Google(Google::generate_contents()),
        );
        let google_provider = InferenceProvider::GoogleGemini;
        let google_capabilities = registry
            .capabilities
            .get(&google_provider)
            .cloned()
            .expect("google capabilities must exist");
        let google_rules = registry
            .rules
            .get(&google_provider)
            .cloned()
            .expect("google rules must exist");
        let converter = TypedEndpointConverter::<
            endpoints::openai::ChatCompletions,
            endpoints::google::GenerateContents,
            GoogleConverter,
        >::new(GoogleConverter::new_with_metadata(
            google_capabilities,
            google_rules,
            model_mapper.clone(),
        ));
        registry.register_converter(key, converter);

        let key = RegistryKey::new(
            ApiEndpoint::OpenAI(OpenAI::chat_completions()),
            ApiEndpoint::OpenAI(OpenAI::chat_completions()),
        );
        let converter =
            TypedEndpointConverter::<
                endpoints::openai::ChatCompletions,
                endpoints::openai::ChatCompletions,
                OpenAIConverter,
            >::new(OpenAIConverter::new(model_mapper.clone()));
        registry.register_converter(key, converter);

        let key = RegistryKey::new(
            ApiEndpoint::OpenAI(OpenAI::chat_completions()),
            ApiEndpoint::Ollama(Ollama::chat_completions()),
        );
        let converter =
            TypedEndpointConverter::<
                endpoints::openai::ChatCompletions,
                endpoints::ollama::chat_completions::ChatCompletions,
                OllamaConverter,
            >::new(OllamaConverter::new(model_mapper.clone()));
        registry.register_converter(key, converter);

        let key = RegistryKey::new(
            ApiEndpoint::OpenAI(OpenAI::chat_completions()),
            ApiEndpoint::Bedrock(Bedrock::converse()),
        );
        let bedrock_provider = InferenceProvider::Bedrock;
        let bedrock_capabilities = registry
            .capabilities
            .get(&bedrock_provider)
            .cloned()
            .expect("bedrock capabilities must exist");
        let bedrock_rules = registry
            .rules
            .get(&bedrock_provider)
            .cloned()
            .expect("bedrock rules must exist");

        let converter = TypedEndpointConverter::<
            endpoints::openai::ChatCompletions,
            endpoints::bedrock::Converse,
            BedrockConverter,
        >::new(BedrockConverter::new_with_metadata(
            bedrock_capabilities,
            bedrock_rules,
            model_mapper.clone(),
        ));

        registry.register_converter(key, converter);

        // OpenAI-family endpoints: same shape source/target; `OpenAIConverter`
        // applies `map_model`.
        let key = RegistryKey::new(
            ApiEndpoint::OpenAI(OpenAI::completions()),
            ApiEndpoint::OpenAI(OpenAI::completions()),
        );
        registry.register_converter(
            key,
            TypedEndpointConverter::<
                endpoints::openai::Completions,
                endpoints::openai::Completions,
                OpenAIConverter,
            >::new(OpenAIConverter::new(model_mapper.clone())),
        );
        let key = RegistryKey::new(
            ApiEndpoint::OpenAI(OpenAI::embeddings()),
            ApiEndpoint::OpenAI(OpenAI::embeddings()),
        );
        registry.register_converter(
            key,
            TypedEndpointConverter::<
                endpoints::openai::Embeddings,
                endpoints::openai::Embeddings,
                OpenAIConverter,
            >::new(OpenAIConverter::new(model_mapper.clone())),
        );
        let key = RegistryKey::new(
            ApiEndpoint::OpenAI(OpenAI::image_generations()),
            ApiEndpoint::OpenAI(OpenAI::image_generations()),
        );
        registry.register_converter(
            key,
            TypedEndpointConverter::<
                endpoints::openai::ImageGenerations,
                endpoints::openai::ImageGenerations,
                OpenAIConverter,
            >::new(OpenAIConverter::new(model_mapper.clone())),
        );
        let key = RegistryKey::new(
            ApiEndpoint::OpenAI(OpenAI::responses()),
            ApiEndpoint::OpenAI(OpenAI::responses()),
        );
        registry.register_converter(
            key,
            TypedEndpointConverter::<
                endpoints::openai::Responses,
                endpoints::openai::Responses,
                OpenAIConverter,
            >::new(OpenAIConverter::new(model_mapper.clone())),
        );

        let key = RegistryKey::new(
            ApiEndpoint::Anthropic(Anthropic::messages()),
            ApiEndpoint::Anthropic(Anthropic::messages()),
        );
        registry.register_converter(
            key,
            TypedEndpointConverter::<
                endpoints::anthropic::Messages,
                endpoints::anthropic::Messages,
                AnthropicConverter,
            >::new(AnthropicConverter::new(model_mapper.clone())),
        );

        registry
    }

    fn register_converter<C>(&mut self, key: RegistryKey, converter: C)
    where
        C: EndpointConverter + Send + Sync + 'static,
    {
        self.converters.insert(key, Box::new(converter));
    }

    fn register_provider_capabilities(
        &mut self,
        provider: InferenceProvider,
    ) -> ProviderCapabilities {
        let capabilities = ProviderCapabilities::for_provider(&provider);
        self.capabilities
            .insert(provider.clone(), capabilities.clone());
        capabilities
    }

    fn register_provider_rules(&mut self, provider: InferenceProvider) {
        let capabilities = self
            .capabilities
            .get(&provider)
            .cloned()
            .unwrap_or_else(|| {
                self.register_provider_capabilities(provider.clone())
            });
        let rules = default_provider_rules(&provider);
        validate_provider_rules(&capabilities, &rules).expect(
            "default provider rules must validate against capabilities",
        );
        self.rules.insert(provider, rules);
    }

    fn register_provider_metadata(&mut self, provider: InferenceProvider) {
        let metadata = resolve_mapper_metadata(&provider, None)
            .expect("embedded provider mapper metadata must validate");
        self.capabilities
            .insert(provider.clone(), metadata.capabilities);
        self.rules.insert(provider, metadata.rules);
    }

    fn register_named_openai_compatible_providers(
        &mut self,
        model_mapper: &ModelMapper,
    ) {
        for provider in model_mapper.configured_providers() {
            if let InferenceProvider::Named(_) = provider {
                self.register_provider_metadata(provider.clone());
                self.register_openai_compatible_converter(
                    provider,
                    model_mapper,
                );
            }
        }
    }

    fn register_openai_compatible_converter(
        &mut self,
        provider: InferenceProvider,
        model_mapper: &ModelMapper,
    ) {
        let capabilities = self
            .capabilities
            .get(&provider)
            .cloned()
            .unwrap_or_else(|| {
                self.register_provider_capabilities(provider.clone())
            });
        let rules = self.rules.get(&provider).cloned().unwrap_or_else(|| {
            self.register_provider_rules(provider.clone());
            self.rules
                .get(&provider)
                .cloned()
                .expect("rules must exist after registration")
        });

        let cc_capabilities = capabilities.clone();
        let cc_rules = rules.clone();
        let embeddings_capabilities = capabilities.clone();
        let embeddings_rules = rules.clone();
        let key = RegistryKey::new(
            ApiEndpoint::OpenAI(OpenAI::chat_completions()),
            ApiEndpoint::OpenAICompatible {
                provider: provider.clone(),
                openai_endpoint: OpenAI::chat_completions(),
            },
        );
        let converter =
            TypedEndpointConverter::<
                endpoints::openai::ChatCompletions,
                endpoints::openai::OpenAICompatibleChatCompletions,
                OpenAICompatibleConverter,
            >::new(OpenAICompatibleConverter::new_with_metadata(
                provider.clone(),
                cc_capabilities,
                cc_rules,
                model_mapper.clone(),
            ));
        self.register_converter(key, converter);

        let key = RegistryKey::new(
            ApiEndpoint::OpenAI(OpenAI::embeddings()),
            ApiEndpoint::OpenAICompatible {
                provider: provider.clone(),
                openai_endpoint: OpenAI::embeddings(),
            },
        );
        let converter =
            TypedEndpointConverter::<
                endpoints::openai::Embeddings,
                endpoints::openai::Embeddings,
                OpenAICompatibleConverter,
            >::new(OpenAICompatibleConverter::new_with_metadata(
                provider.clone(),
                embeddings_capabilities,
                embeddings_rules,
                model_mapper.clone(),
            ));
        self.register_converter(key, converter);

        let key = RegistryKey::new(
            ApiEndpoint::OpenAI(OpenAI::responses()),
            ApiEndpoint::OpenAICompatible {
                provider: provider.clone(),
                openai_endpoint: OpenAI::responses(),
            },
        );
        let converter =
            TypedEndpointConverter::<
                endpoints::openai::Responses,
                endpoints::openai::Responses,
                OpenAICompatibleConverter,
            >::new(OpenAICompatibleConverter::new_with_metadata(
                provider,
                capabilities,
                rules,
                model_mapper.clone(),
            ));
        self.register_converter(key, converter);
    }
}

#[cfg(test)]
mod capability_tests {
    use std::sync::Arc;

    use crate::{
        app::build_test_app, config::Config,
        middleware::mapper::model::ModelMapper,
        types::provider::InferenceProvider,
    };

    #[test]
    fn deepseek_is_marked_as_openai_compatible() {
        let capabilities =
            crate::middleware::mapper::capabilities::ProviderCapabilities::for_provider(
                &InferenceProvider::Named("deepseek".into()),
            );

        assert!(capabilities.openai_compatible);
        assert!(capabilities.supports_tool_choice);
    }

    #[test]
    fn registry_returns_provider_rules() {
        let provider = InferenceProvider::Named("deepseek".into());
        let mut inner = super::EndpointConverterRegistryInner::default();
        inner.register_provider_capabilities(provider.clone());
        inner.register_provider_rules(provider.clone());
        let registry = super::EndpointConverterRegistry(Arc::new(inner));

        let rules = registry
            .get_provider_rules(&provider)
            .expect("rules should exist");

        assert_eq!(
            rules.family,
            crate::middleware::mapper::families::ProviderProtocolFamily::OpenAiCompatible
        );
    }

    #[test]
    fn registry_returns_provider_rules_for_anthropic() {
        let provider = InferenceProvider::Anthropic;
        let mut inner = super::EndpointConverterRegistryInner::default();
        inner.register_provider_metadata(provider.clone());
        let registry = super::EndpointConverterRegistry(Arc::new(inner));

        let rules = registry
            .get_provider_rules(&provider)
            .expect("rules should exist");

        assert_eq!(
            rules.family,
            crate::middleware::mapper::families::ProviderProtocolFamily::AnthropicMessages
        );
    }

    #[test]
    fn registry_register_provider_metadata_populates_capabilities_and_rules() {
        let provider = InferenceProvider::Named("deepseek".into());
        let mut inner = super::EndpointConverterRegistryInner::default();

        inner.register_provider_metadata(provider.clone());

        let registry = super::EndpointConverterRegistry(Arc::new(inner));
        assert!(registry.get_provider_capabilities(&provider).is_some());
        assert!(registry.get_provider_rules(&provider).is_some());
    }

    #[tokio::test]
    async fn registry_registers_named_provider_from_runtime_config() {
        let mut config = Config::default();
        let provider = InferenceProvider::Named("custom-openai".into());
        let template = config
            .providers
            .get(&InferenceProvider::Named("deepseek".into()))
            .expect("deepseek provider should exist")
            .clone();
        config.providers.insert(provider.clone(), template);

        let app = build_test_app(config).await.expect("build app");
        let model_mapper = ModelMapper::new(app.state.clone());
        let registry = super::EndpointConverterRegistry::new(&model_mapper);

        assert!(registry.get_provider_capabilities(&provider).is_some());
        assert!(registry.get_provider_rules(&provider).is_some());
        assert!(registry
            .get_converter(
                &crate::endpoints::ApiEndpoint::OpenAI(
                    crate::endpoints::openai::OpenAI::chat_completions()
                ),
                &crate::endpoints::ApiEndpoint::OpenAICompatible {
                    provider: provider.clone(),
                    openai_endpoint: crate::endpoints::openai::OpenAI::chat_completions(),
                }
            )
            .is_some());
    }

    #[tokio::test]
    async fn registry_registers_named_provider_responses_converter() {
        let mut config = Config::default();
        let provider = InferenceProvider::Named("custom-openai".into());
        let template = config
            .providers
            .get(&InferenceProvider::Named("deepseek".into()))
            .expect("deepseek provider should exist")
            .clone();
        config.providers.insert(provider.clone(), template);

        let app = build_test_app(config).await.expect("build app");
        let model_mapper = ModelMapper::new(app.state.clone());
        let registry = super::EndpointConverterRegistry::new(&model_mapper);

        assert!(
            registry
                .get_converter(
                    &crate::endpoints::ApiEndpoint::OpenAI(
                        crate::endpoints::openai::OpenAI::responses()
                    ),
                    &crate::endpoints::ApiEndpoint::OpenAICompatible {
                        provider: provider.clone(),
                        openai_endpoint:
                            crate::endpoints::openai::OpenAI::responses(),
                    }
                )
                .is_some(),
            "Responses converter should be registered for Named provider"
        );
    }

    #[tokio::test]
    async fn registry_registers_named_provider_embeddings_converter() {
        let mut config = Config::default();
        let provider = InferenceProvider::Named("custom-openai".into());
        let template = config
            .providers
            .get(&InferenceProvider::Named("deepseek".into()))
            .expect("deepseek provider should exist")
            .clone();
        config.providers.insert(provider.clone(), template);

        let app = build_test_app(config).await.expect("build app");
        let model_mapper = ModelMapper::new(app.state.clone());
        let registry = super::EndpointConverterRegistry::new(&model_mapper);

        assert!(
            registry
                .get_converter(
                    &crate::endpoints::ApiEndpoint::OpenAI(
                        crate::endpoints::openai::OpenAI::embeddings()
                    ),
                    &crate::endpoints::ApiEndpoint::OpenAICompatible {
                        provider: provider.clone(),
                        openai_endpoint:
                            crate::endpoints::openai::OpenAI::embeddings(),
                    }
                )
                .is_some(),
            "Embeddings converter should be registered for Named provider"
        );
    }
}
