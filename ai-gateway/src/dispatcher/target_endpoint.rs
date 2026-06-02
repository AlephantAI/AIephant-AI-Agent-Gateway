use std::borrow::Cow;

use crate::{
    app_state::AppState,
    error::{api::ApiError, internal::InternalError},
    types::{
        extensions::{AuthContext, RequestContext},
        provider::InferenceProvider,
    },
};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
#[allow(dead_code)]
pub(crate) enum TargetEndpointSource {
    MasterKeyBaseUrl,
    RouterProviderBaseUrl,
    LearnedCn,
    GlobalProviderBaseUrl,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct TargetEndpoint {
    pub(crate) url: url::Url,
    pub(crate) source: TargetEndpointSource,
    pub(crate) cn_retry_url: Option<url::Url>,
}

#[allow(dead_code)]
pub(crate) struct TargetEndpointRequest<'a> {
    pub(crate) request_context: &'a RequestContext,
    pub(crate) target_provider: &'a InferenceProvider,
    pub(crate) path_and_query: &'a str,
    pub(crate) allow_learned_region: bool,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct TargetEndpointResolver {
    app_state: AppState,
}

impl TargetEndpointResolver {
    #[must_use]
    #[allow(dead_code)]
    pub(crate) fn new(app_state: AppState) -> Self {
        Self { app_state }
    }

    #[allow(dead_code)]
    pub(crate) async fn resolve(
        &self,
        request: TargetEndpointRequest<'_>,
    ) -> Result<TargetEndpoint, ApiError> {
        if let Some(master_url) = resolve_master_key_target_url(
            request.request_context.auth_context.as_ref(),
            request.path_and_query,
        ) {
            return Ok(TargetEndpoint {
                url: master_url,
                source: TargetEndpointSource::MasterKeyBaseUrl,
                cn_retry_url: None,
            });
        }

        if let Some(router_config) =
            request.request_context.router_config.as_ref()
            && let Some(router_provider_config) =
                router_config.providers.as_ref()
            && let Some(provider_config) =
                router_provider_config.get(request.target_provider)
        {
            return Ok(TargetEndpoint {
                url: join_provider_upstream_url(
                    &provider_config.base_url,
                    request.path_and_query,
                ),
                source: TargetEndpointSource::RouterProviderBaseUrl,
                cn_retry_url: None,
            });
        }

        let providers_config = self.app_state.get_providers_config();
        let selected_provider_config = providers_config
            .get(request.target_provider)
            .ok_or_else(|| {
                InternalError::ProviderNotConfigured(
                    request.target_provider.clone(),
                )
            })?;
        let base_url = selected_provider_config.base_url.clone();
        let cn_retry_url =
            selected_provider_config
                .cn_base_url
                .clone()
                .map(|cn_base_url| {
                    join_provider_upstream_url(
                        &cn_base_url,
                        request.path_and_query,
                    )
                });
        drop(providers_config);

        let learned_region = if request.allow_learned_region
            && let Some(master_key_id) = request
                .request_context
                .auth_context
                .as_ref()
                .and_then(|auth_ctx| auth_ctx.master_key_id)
            && cn_retry_url.is_some()
        {
            crate::dispatcher::regional_endpoint::get_learned_region(
                &self.app_state,
                Some(master_key_id),
            )
            .await
        } else {
            None
        };

        if matches!(
            learned_region,
            Some(crate::dispatcher::regional_endpoint::EndpointRegion::Cn)
        ) && let Some(cn_url) = cn_retry_url.clone()
        {
            return Ok(TargetEndpoint {
                url: cn_url,
                source: TargetEndpointSource::LearnedCn,
                cn_retry_url,
            });
        }

        Ok(TargetEndpoint {
            url: join_provider_upstream_url(&base_url, request.path_and_query),
            source: TargetEndpointSource::GlobalProviderBaseUrl,
            cn_retry_url,
        })
    }
}

/// True when the last non-empty path segment is `v` + ASCII digits, e.g.
/// `v1` or `V4`.
fn base_path_ends_with_v_version_segment(url: &url::Url) -> bool {
    let path = url.path().trim_matches('/');
    if path.is_empty() {
        return false;
    }
    let last = path.rsplit('/').next().unwrap_or("");
    segment_is_v_digit_version(last)
}

fn segment_is_v_digit_version(segment: &str) -> bool {
    let b = segment.as_bytes();
    if b.len() < 2 {
        return false;
    }
    if !matches!(b[0], b'v' | b'V') {
        return false;
    }
    b[1..].iter().all(u8::is_ascii_digit)
}

fn strip_leading_numeric_v_revision_prefix(path: &str) -> Option<&str> {
    let path = path.trim_start_matches('/');
    let (first, rest) = path.split_once('/')?;
    if rest.is_empty() {
        return None;
    }
    if !segment_is_v_digit_version(first) {
        return None;
    }
    Some(rest)
}

fn adjust_upstream_path_for_versioned_base<'a>(
    base_url: &url::Url,
    path_and_query: &'a str,
) -> Cow<'a, str> {
    if !base_path_ends_with_v_version_segment(base_url) {
        return Cow::Borrowed(path_and_query);
    }
    let (path_only, query) = match path_and_query.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (path_and_query, None),
    };
    let Some(stripped) = strip_leading_numeric_v_revision_prefix(path_only)
    else {
        return Cow::Borrowed(path_and_query);
    };
    let mut out = stripped.to_string();
    if let Some(q) = query {
        out.push('?');
        out.push_str(q);
    }
    Cow::Owned(out)
}

pub(crate) fn join_provider_upstream_url(
    base_url: &url::Url,
    path_and_query: &str,
) -> url::Url {
    let adjusted =
        adjust_upstream_path_for_versioned_base(base_url, path_and_query);
    let slice = match &adjusted {
        Cow::Borrowed(s) => *s,
        Cow::Owned(s) => s.as_str(),
    };
    let mut normalized_base = base_url.clone();
    if base_path_ends_with_v_version_segment(&normalized_base)
        && !normalized_base.path().ends_with('/')
    {
        let with_trailing_slash = format!("{}/", normalized_base.path());
        normalized_base.set_path(&with_trailing_slash);
    }
    normalized_base
        .join(slice)
        .expect("PathAndQuery joined with valid url will always succeed")
}

pub(crate) fn resolve_master_key_target_url(
    auth_ctx: Option<&AuthContext>,
    path_and_query: &str,
) -> Option<url::Url> {
    let base_url_str = auth_ctx?.master_key_base_url.as_ref()?;
    if let Ok(base_url) = url::Url::parse(base_url_str) {
        Some(join_provider_upstream_url(&base_url, path_and_query))
    } else {
        tracing::warn!(
            base_url = %base_url_str,
            "master_key base_url is not a valid URL, falling through"
        );
        None
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Arc};

    use uuid::Uuid;

    use super::*;
    use crate::{
        app::build_test_app,
        config::{
            Config,
            router::{RouterConfig, RouterProviderConfig},
        },
        types::{
            extensions::AuthContext, org::OrgId, secret::Secret, user::UserId,
        },
    };

    fn auth_ctx_with_base_url(base_url: Option<&str>) -> AuthContext {
        AuthContext {
            api_key: Secret::from("sk-test".to_string()),
            user_id: UserId::new(Uuid::new_v4()),
            org_id: OrgId::new(Uuid::new_v4()),
            workspace_type: None,
            virtual_key_id: Some(Uuid::new_v4()),
            virtual_key_prefix: String::new(),
            master_key_id: Some(Uuid::new_v4()),
            master_key_base_url: base_url.map(ToOwned::to_owned),
            department_id: Uuid::nil(),
            entity_type: String::new(),
            entity_id: Uuid::nil(),
            entity_name: String::new(),
            registered_agent_name: None,
            body_ttl_days: 90,
            is_custom_provider: false,
            master_key_allowed_providers: None,
        }
    }

    fn request_ctx(
        auth_context: Option<AuthContext>,
        router_config: Option<RouterConfig>,
    ) -> RequestContext {
        RequestContext {
            auth_context,
            router_config: router_config.map(Arc::new),
            llm_kv_cache_read_allowed: true,
            llm_kv_cache_write_allowed: true,
            agent_context: None,
        }
    }

    async fn resolver(config: Config) -> TargetEndpointResolver {
        let app = build_test_app(config).await.expect("build app");
        TargetEndpointResolver::new(app.state)
    }

    #[test]
    fn resolve_master_key_target_url_uses_valid_override() {
        let auth = auth_ctx_with_base_url(Some("https://example.com"));
        let url = resolve_master_key_target_url(Some(&auth), "/v1/chat")
            .expect("valid url expected");
        assert_eq!(url.as_str(), "https://example.com/v1/chat");
    }

    #[test]
    fn resolve_master_key_target_url_returns_none_for_invalid_override() {
        let auth = auth_ctx_with_base_url(Some("not a url"));
        let url = resolve_master_key_target_url(Some(&auth), "/v1/chat");
        assert!(url.is_none());
    }

    #[test]
    fn resolve_master_key_target_url_returns_none_when_absent() {
        let auth = auth_ctx_with_base_url(None);
        let url = resolve_master_key_target_url(Some(&auth), "/v1/chat");
        assert!(url.is_none());
    }

    #[test]
    fn join_url_strips_api_revision_from_versioned_base() {
        let base_url: url::Url =
            "https://open.bigmodel.cn/api/paas/v4/".parse().unwrap();

        for path in ["v1/chat/completions", "/v1/chat/completions"] {
            let target_url = join_provider_upstream_url(&base_url, path);
            assert_eq!(
                target_url.as_str(),
                "https://open.bigmodel.cn/api/paas/v4/chat/completions",
                "path={path:?}",
            );
        }
    }

    #[test]
    fn join_url_strips_v_prefix_for_master_key_base() {
        let base_url: url::Url =
            "https://mk.example/api/service/v2/".parse().unwrap();

        let target_url =
            join_provider_upstream_url(&base_url, "v1/embeddings?user=a");

        assert_eq!(
            target_url.as_str(),
            "https://mk.example/api/service/v2/embeddings?user=a"
        );
    }

    #[test]
    fn join_url_keeps_revision_without_trailing_slash() {
        let base_url: url::Url =
            "https://open.bigmodel.cn/api/paas/v4".parse().unwrap();

        let target_url =
            join_provider_upstream_url(&base_url, "v1/chat/completions");

        assert_eq!(
            target_url.as_str(),
            "https://open.bigmodel.cn/api/paas/v4/chat/completions"
        );
    }

    #[tokio::test]
    async fn resolve_prefers_master_key_base_url_over_router_and_global() {
        let mut config = Config::default();
        config
            .providers
            .get_mut(&InferenceProvider::OpenAI)
            .expect("openai config")
            .base_url = "https://global-provider.test/".parse().unwrap();
        let resolver = resolver(config).await;

        let req_ctx = request_ctx(
            Some(auth_ctx_with_base_url(Some("https://master-key.test/"))),
            Some(RouterConfig {
                providers: Some(HashMap::from([(
                    InferenceProvider::OpenAI,
                    RouterProviderConfig {
                        base_url: "https://router-provider.test/"
                            .parse()
                            .unwrap(),
                        version: None,
                    },
                )])),
                ..RouterConfig::default()
            }),
        );

        let endpoint = resolver
            .resolve(TargetEndpointRequest {
                request_context: &req_ctx,
                target_provider: &InferenceProvider::OpenAI,
                path_and_query: "/v1/chat/completions",
                allow_learned_region: true,
            })
            .await
            .expect("target endpoint");

        assert_eq!(
            endpoint.url.as_str(),
            "https://master-key.test/v1/chat/completions"
        );
        assert_eq!(endpoint.source, TargetEndpointSource::MasterKeyBaseUrl);
        assert_eq!(endpoint.cn_retry_url, None);
    }

    #[tokio::test]
    async fn resolve_uses_router_provider_base_url() {
        let mut config = Config::default();
        config
            .providers
            .get_mut(&InferenceProvider::OpenAI)
            .expect("openai config")
            .base_url = "https://global-provider.test/".parse().unwrap();
        let resolver = resolver(config).await;

        let req_ctx = request_ctx(
            Some(auth_ctx_with_base_url(None)),
            Some(RouterConfig {
                providers: Some(HashMap::from([(
                    InferenceProvider::OpenAI,
                    RouterProviderConfig {
                        base_url: "https://router-provider.test/"
                            .parse()
                            .unwrap(),
                        version: None,
                    },
                )])),
                ..RouterConfig::default()
            }),
        );

        let endpoint = resolver
            .resolve(TargetEndpointRequest {
                request_context: &req_ctx,
                target_provider: &InferenceProvider::OpenAI,
                path_and_query: "/v1/chat/completions",
                allow_learned_region: true,
            })
            .await
            .expect("target endpoint");

        assert_eq!(
            endpoint.url.as_str(),
            "https://router-provider.test/v1/chat/completions"
        );
        assert_eq!(
            endpoint.source,
            TargetEndpointSource::RouterProviderBaseUrl
        );
        assert_eq!(endpoint.cn_retry_url, None);
    }

    #[tokio::test]
    async fn resolve_falls_back_to_global_provider_config_when_router_absent() {
        let mut config = Config::default();
        config
            .providers
            .get_mut(&InferenceProvider::OpenAI)
            .expect("openai config")
            .base_url = "https://global-provider.test/".parse().unwrap();
        let resolver = resolver(config).await;
        let req_ctx = request_ctx(Some(auth_ctx_with_base_url(None)), None);

        let endpoint = resolver
            .resolve(TargetEndpointRequest {
                request_context: &req_ctx,
                target_provider: &InferenceProvider::OpenAI,
                path_and_query: "/v1/chat/completions",
                allow_learned_region: true,
            })
            .await
            .expect("target endpoint");

        assert_eq!(
            endpoint.url.as_str(),
            "https://global-provider.test/v1/chat/completions"
        );
        assert_eq!(
            endpoint.source,
            TargetEndpointSource::GlobalProviderBaseUrl
        );
    }

    #[tokio::test]
    async fn resolve_errors_when_global_provider_missing() {
        let mut config = Config::default();
        config.providers.shift_remove(&InferenceProvider::OpenAI);
        let resolver = resolver(config).await;
        let req_ctx = request_ctx(Some(auth_ctx_with_base_url(None)), None);

        let error = resolver
            .resolve(TargetEndpointRequest {
                request_context: &req_ctx,
                target_provider: &InferenceProvider::OpenAI,
                path_and_query: "/v1/chat/completions",
                allow_learned_region: true,
            })
            .await
            .expect_err("missing provider config should fail");

        assert!(matches!(
            error,
            ApiError::Internal(InternalError::ProviderNotConfigured(
                InferenceProvider::OpenAI
            ))
        ));
    }

    #[tokio::test]
    async fn resolve_uses_learned_cn_when_available() {
        let mut config = Config::default();
        let openai_config = config
            .providers
            .get_mut(&InferenceProvider::OpenAI)
            .expect("openai config");
        openai_config.base_url = "https://global.openai.test/".parse().unwrap();
        openai_config.cn_base_url =
            Some("https://cn.openai.test/".parse().unwrap());
        let resolver = resolver(config).await;
        let auth = auth_ctx_with_base_url(None);
        let master_key_id = auth.master_key_id;
        let req_ctx = request_ctx(Some(auth), None);
        crate::dispatcher::regional_endpoint::remember_region(
            &resolver.app_state,
            master_key_id,
            crate::dispatcher::regional_endpoint::EndpointRegion::Cn,
        )
        .await;

        let endpoint = resolver
            .resolve(TargetEndpointRequest {
                request_context: &req_ctx,
                target_provider: &InferenceProvider::OpenAI,
                path_and_query: "/v1/chat/completions",
                allow_learned_region: true,
            })
            .await
            .expect("target endpoint");

        assert_eq!(
            endpoint.url.as_str(),
            "https://cn.openai.test/v1/chat/completions"
        );
        assert_eq!(endpoint.source, TargetEndpointSource::LearnedCn);
        assert_eq!(
            endpoint.cn_retry_url.as_ref().map(url::Url::as_str),
            Some("https://cn.openai.test/v1/chat/completions")
        );
    }

    #[tokio::test]
    async fn resolve_ignores_learned_cn_when_region_learning_disabled() {
        let mut config = Config::default();
        let openai_config = config
            .providers
            .get_mut(&InferenceProvider::OpenAI)
            .expect("openai config");
        openai_config.base_url = "https://global.openai.test/".parse().unwrap();
        openai_config.cn_base_url =
            Some("https://cn.openai.test/".parse().unwrap());
        let resolver = resolver(config).await;
        let auth = auth_ctx_with_base_url(None);
        let master_key_id = auth.master_key_id;
        let req_ctx = request_ctx(Some(auth), None);
        crate::dispatcher::regional_endpoint::remember_region(
            &resolver.app_state,
            master_key_id,
            crate::dispatcher::regional_endpoint::EndpointRegion::Cn,
        )
        .await;

        let endpoint = resolver
            .resolve(TargetEndpointRequest {
                request_context: &req_ctx,
                target_provider: &InferenceProvider::OpenAI,
                path_and_query: "/v1/chat/completions",
                allow_learned_region: false,
            })
            .await
            .expect("target endpoint");

        assert_eq!(
            endpoint.url.as_str(),
            "https://global.openai.test/v1/chat/completions"
        );
        assert_eq!(
            endpoint.source,
            TargetEndpointSource::GlobalProviderBaseUrl
        );
        assert_eq!(
            endpoint.cn_retry_url.as_ref().map(url::Url::as_str),
            Some("https://cn.openai.test/v1/chat/completions")
        );
    }

    #[tokio::test]
    async fn resolve_prefers_master_key_base_url_over_learned_cn() {
        let mut config = Config::default();
        let openai_config = config
            .providers
            .get_mut(&InferenceProvider::OpenAI)
            .expect("openai config");
        openai_config.base_url = "https://global.openai.test/".parse().unwrap();
        openai_config.cn_base_url =
            Some("https://cn.openai.test/".parse().unwrap());
        let resolver = resolver(config).await;
        let auth = auth_ctx_with_base_url(Some("https://master-key.test/"));
        let master_key_id = auth.master_key_id;
        let req_ctx = request_ctx(Some(auth), None);
        crate::dispatcher::regional_endpoint::remember_region(
            &resolver.app_state,
            master_key_id,
            crate::dispatcher::regional_endpoint::EndpointRegion::Cn,
        )
        .await;

        let endpoint = resolver
            .resolve(TargetEndpointRequest {
                request_context: &req_ctx,
                target_provider: &InferenceProvider::OpenAI,
                path_and_query: "/v1/chat/completions",
                allow_learned_region: true,
            })
            .await
            .expect("target endpoint");

        assert_eq!(
            endpoint.url.as_str(),
            "https://master-key.test/v1/chat/completions"
        );
        assert_eq!(endpoint.source, TargetEndpointSource::MasterKeyBaseUrl);
        assert_eq!(endpoint.cn_retry_url, None);
    }
}
