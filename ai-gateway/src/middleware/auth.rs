use axum_core::response::IntoResponse;
use chrono::Utc;
use futures::future::BoxFuture;
use http::Request;
use tower_http::auth::AsyncAuthorizeRequest;
use uuid::Uuid;

use crate::{
    app_state::AppState,
    error::{
        api::ApiError, auth::AuthError, internal::InternalError,
        invalid_req::InvalidRequestError,
    },
    store::{
        enrichment_redis::{
            CachedDepartmentEnrichment, ENRICHMENT_CACHE_TTL_SECS,
            enrichment_cache_key,
        },
        router::DbVirtualKey,
    },
    types::{
        extensions::{AuthContext, RequestKind, VkPolicy},
        org::OrgId,
        provider::InferenceProvider,
        router::RouterId,
        secret::Secret,
        user::UserId,
    },
    virtual_key::legacy_key::hash_key,
};

#[derive(Clone)]
pub struct AuthService {
    app_state: AppState,
}

/// After VK validation: load agent/member `department_id` when possible.
/// Any failure or missing row → [`Uuid::nil()`]; does not affect auth success.
async fn resolve_department_id_after_vk_auth(
    app_state: &AppState,
    vk_id: Uuid,
) -> Uuid {
    let default = Uuid::nil();
    let cache_key = enrichment_cache_key(vk_id);
    if let Some(redis) = app_state.redis() {
        match redis.get_string(&cache_key).await {
            Ok(Some(raw)) => {
                if let Ok(cached) =
                    serde_json::from_str::<CachedDepartmentEnrichment>(&raw)
                {
                    return cached.department_id.unwrap_or(default);
                }
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    %vk_id,
                    "auth: enrichment redis get failed; falling back to PG"
                );
            }
        }
    }

    let Some(store) = app_state.router_store() else {
        return default;
    };
    match store.fetch_request_log_department_enrichment(vk_id).await {
        Ok(Some(row)) => {
            let resolved = row.department_id.unwrap_or(default);
            if let Some(redis) = app_state.redis() {
                let payload = CachedDepartmentEnrichment {
                    department_id: row.department_id,
                };
                match serde_json::to_string(&payload) {
                    Ok(json) => {
                        if let Err(e) = redis
                            .set_ex(
                                &cache_key,
                                &json,
                                ENRICHMENT_CACHE_TTL_SECS,
                            )
                            .await
                        {
                            tracing::warn!(
                                error = %e,
                                %vk_id,
                                "auth: enrichment redis set_ex failed"
                            );
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            %vk_id,
                            "auth: enrichment cache serialize failed"
                        );
                    }
                }
            }
            resolved
        }
        Ok(None) => default,
        Err(e) => {
            tracing::warn!(
                error = %e,
                %vk_id,
                "auth: department lookup failed; using default department_id"
            );
            default
        }
    }
}

impl AuthService {
    #[must_use]
    pub fn new(app_state: AppState) -> Self {
        Self { app_state }
    }

    async fn authenticate_request_inner(
        app_state: AppState,
        api_key: &str,
        request_kind: Option<&RequestKind>,
        router_id: Option<&RouterId>,
    ) -> Result<(AuthContext, Option<VkPolicy>), ApiError> {
        let api_key_without_bearer = strip_bearer_prefix(api_key).to_string();
        let computed_hash = hash_key(&api_key_without_bearer);

        let Some(request_kind) = request_kind else {
            return Err(InternalError::ExtensionNotFound("RequestKind").into());
        };

        // Look up the virtual key by hash (memory, then optional PG fallback).
        let Some(vk) =
            app_state.resolve_virtual_key_for_auth(&computed_hash).await
        else {
            return Err(AuthError::InvalidCredentials.into());
        };

        // Reject expired keys.
        if virtual_key_is_expired(&vk, Utc::now()) {
            return Err(AuthError::InvalidCredentials.into());
        }

        let (owner_id, org_id) = virtual_key_owner_and_org(&vk);

        // Resolve master_key base_url from the LRU cache (usually a fast
        // in-process lookup; `None` on cache miss or no custom URL).
        let (
            master_key_base_url,
            is_custom_provider,
            master_key_allowed_providers,
        ) = if let Some(cache) = app_state.0.master_key_cache.as_ref() {
            match cache.get(vk.master_key_id).await {
                Ok(dk) => (
                    dk.base_url,
                    matches!(dk.provider, InferenceProvider::Custom),
                    Some(vec![dk.provider.clone()]),
                ),
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        master_key_id = %vk.master_key_id,
                        "auth: could not resolve master_key"
                    );
                    (None, false, None)
                }
            }
        } else {
            (None, false, None)
        };

        if is_custom_provider && master_key_base_url.is_none() {
            return Err(
                InvalidRequestError::CustomProviderMissingBaseUrl.into()
            );
        }

        let vk_policy = VkPolicy {
            virtual_key_id: vk.id,
            allowed_models: vk.allowed_models.clone(),
            blocked_models: vk.blocked_models.clone(),
        };

        let (entity_type, entity_id, entity_name) =
            auth_entity_scope_from_vk(&vk);
        let department_id =
            resolve_department_id_after_vk_auth(&app_state, vk.id).await;
        let body_ttl_days = body_ttl_days_from_subscription_log_limit(
            vk.subscription_log_limit,
        );

        match request_kind {
            RequestKind::Router => {
                let Some(router_id) = router_id else {
                    return Err(
                        InternalError::ExtensionNotFound("RouterId").into()
                    );
                };

                let Some(router_organization_id) =
                    app_state.get_router_organization(router_id).await
                else {
                    return Err(InvalidRequestError::NotFound(
                        "router not found".to_string(),
                    )
                    .into());
                };

                if router_organization_id == org_id {
                    Ok((
                        AuthContext {
                            api_key: Secret::from(api_key_without_bearer),
                            user_id: owner_id,
                            org_id,
                            virtual_key_id: Some(vk.id),
                            virtual_key_prefix: vk.key_prefix.clone(),
                            master_key_id: Some(vk.master_key_id),
                            master_key_base_url,
                            department_id,
                            entity_type,
                            entity_id,
                            entity_name,
                            body_ttl_days,
                            is_custom_provider,
                            master_key_allowed_providers,
                        },
                        Some(vk_policy),
                    ))
                } else {
                    Err(AuthError::InvalidCredentials.into())
                }
            }
            RequestKind::UnifiedApi
            | RequestKind::DirectProxy
            | RequestKind::CustomProvider => Ok((
                AuthContext {
                    api_key: Secret::from(api_key_without_bearer),
                    user_id: owner_id,
                    org_id,
                    virtual_key_id: Some(vk.id),
                    virtual_key_prefix: vk.key_prefix.clone(),
                    master_key_id: Some(vk.master_key_id),
                    master_key_base_url,
                    department_id,
                    entity_type,
                    entity_id,
                    entity_name,
                    body_ttl_days,
                    is_custom_provider,
                    master_key_allowed_providers,
                },
                Some(vk_policy),
            )),
        }
    }
}

fn strip_bearer_prefix(api_key: &str) -> &str {
    api_key.strip_prefix("Bearer ").unwrap_or(api_key)
}

/// Maps `subscriptions.log_limit` into RMT `body_ttl_days` (1–730); otherwise
/// default **90** (also used when the value is outside the allowed range).
#[must_use]
pub(crate) fn body_ttl_days_from_subscription_log_limit(
    subscription_log_limit: i32,
) -> u16 {
    if (1..=730).contains(&subscription_log_limit) {
        u16::try_from(subscription_log_limit).unwrap_or(90)
    } else {
        90
    }
}

fn virtual_key_owner_and_org(vk: &DbVirtualKey) -> (UserId, OrgId) {
    (
        UserId::new(vk.entity_id.unwrap_or(vk.workspace_id)),
        OrgId::new(vk.workspace_id),
    )
}

/// VK row fields used for logs / RMT entity columns (`label` → display name).
fn auth_entity_scope_from_vk(vk: &DbVirtualKey) -> (String, Uuid, String) {
    (
        vk.entity_type.clone().unwrap_or_default(),
        vk.entity_id.unwrap_or(Uuid::nil()),
        vk.label.clone(),
    )
}

fn virtual_key_is_expired(
    vk: &DbVirtualKey,
    now: chrono::DateTime<Utc>,
) -> bool {
    vk.expires_at.is_some_and(|expires_at| now >= expires_at)
}

impl<B> AsyncAuthorizeRequest<B> for AuthService
where
    B: Send + 'static,
{
    type RequestBody = B;
    type ResponseBody = axum_core::body::Body;
    type Future = BoxFuture<
        'static,
        Result<Request<B>, http::Response<Self::ResponseBody>>,
    >;

    #[tracing::instrument(skip_all)]
    fn authorize(&mut self, mut request: Request<B>) -> Self::Future {
        let app_state = self.app_state.clone();
        Box::pin(async move {
            if app_state.0.config.alephant.is_auth_disabled() {
                tracing::trace!("auth middleware: auth disabled");
                return Ok(request);
            }
            tracing::trace!("auth middleware");
            let Some(api_key) = request
                .headers()
                .get("authorization")
                .and_then(|h| h.to_str().ok())
            else {
                return Err(
                    AuthError::MissingAuthorizationHeader.into_response()
                );
            };
            app_state.0.metrics.auth_attempts.add(1, &[]);

            let request_kind = request.extensions().get::<RequestKind>();
            let router_id = request.extensions().get::<RouterId>();

            match Self::authenticate_request_inner(
                app_state.clone(),
                api_key,
                request_kind,
                router_id,
            )
            .await
            {
                Ok((auth_ctx, vk_policy)) => {
                    tracing::info!(
                        master_key_id = ?auth_ctx.master_key_id,
                        master_key_base_url = ?auth_ctx.master_key_base_url,
                        is_custom_provider = auth_ctx.is_custom_provider,
                        vk_prefix = %auth_ctx.virtual_key_prefix,
                        "auth: authenticated, master_key info"
                    );
                    if auth_ctx.is_custom_provider {
                        request
                            .extensions_mut()
                            .insert(RequestKind::CustomProvider);
                    }
                    request.extensions_mut().insert(auth_ctx);
                    if let Some(policy) = vk_policy {
                        request.extensions_mut().insert(policy);
                    }
                    Ok(request)
                }
                Err(e) => {
                    if let ApiError::Authentication(auth_error) = &e {
                        match auth_error {
                            AuthError::MissingAuthorizationHeader
                            | AuthError::InvalidCredentials
                            | AuthError::ProviderKeyNotFound => {
                                app_state.0.metrics.auth_rejections.add(1, &[]);
                            }
                        }
                    }
                    Err(e.into_response())
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};
    use uuid::Uuid;

    use super::*;
    use crate::{app::build_test_app, config::Config};

    fn sample_vk(
        entity_id: Option<Uuid>,
        expires_at: Option<chrono::DateTime<Utc>>,
    ) -> DbVirtualKey {
        DbVirtualKey {
            id: Uuid::new_v4(),
            workspace_id: Uuid::new_v4(),
            master_key_id: Uuid::new_v4(),
            key_hash: "kh".to_string(),
            key_prefix: "vk-test".to_string(),
            label: "member:test".to_string(),
            entity_type: Some("user".to_string()),
            entity_id,
            status: "active".to_string(),
            expires_at,
            deleted_at: None,
            updated_at: Utc::now(),
            rate_limit_rpm: None,
            rate_limit_rph: None,
            allowed_models: None,
            blocked_models: None,
            subscription_log_limit: 90,
        }
    }

    #[test]
    fn strip_bearer_prefix_only_removes_prefix() {
        assert_eq!(strip_bearer_prefix("Bearer sk-test"), "sk-test");
        assert_eq!(strip_bearer_prefix("sk-Bearer-test"), "sk-Bearer-test");
    }

    #[test]
    fn auth_entity_scope_maps_virtual_key_row() {
        let entity = Uuid::new_v4();
        let vk = sample_vk(Some(entity), None);
        let (entity_type, entity_id, entity_name) =
            super::auth_entity_scope_from_vk(&vk);
        assert_eq!(entity_type, "user");
        assert_eq!(entity_id, entity);
        assert_eq!(entity_name, "member:test");
    }

    #[test]
    fn virtual_key_owner_uses_entity_id_when_present() {
        let entity = Uuid::new_v4();
        let vk = sample_vk(Some(entity), None);
        let (owner, org) = virtual_key_owner_and_org(&vk);
        assert_eq!(owner.as_ref(), &entity);
        assert_eq!(org.as_ref(), &vk.workspace_id);
    }

    #[test]
    fn virtual_key_owner_falls_back_to_workspace_id() {
        let vk = sample_vk(None, None);
        let (owner, org) = virtual_key_owner_and_org(&vk);
        assert_eq!(owner.as_ref(), &vk.workspace_id);
        assert_eq!(org.as_ref(), &vk.workspace_id);
    }

    #[test]
    fn virtual_key_expiration_checks() {
        let now = Utc::now();
        let no_expiry = sample_vk(None, None);
        let future = sample_vk(None, Some(now + Duration::minutes(1)));
        let past = sample_vk(None, Some(now - Duration::minutes(1)));
        let at_now = sample_vk(None, Some(now));

        assert!(!virtual_key_is_expired(&no_expiry, now));
        assert!(!virtual_key_is_expired(&future, now));
        assert!(virtual_key_is_expired(&past, now));
        assert!(virtual_key_is_expired(&at_now, now));
    }

    #[tokio::test]
    async fn authenticate_inner_rejects_unknown_virtual_key() {
        let app = build_test_app(Config::default()).await.expect("build app");
        let err = AuthService::authenticate_request_inner(
            app.state.clone(),
            "Bearer sk-unknown",
            Some(&RequestKind::UnifiedApi),
            None,
        )
        .await
        .expect_err("missing key should be rejected");
        assert!(matches!(
            err,
            ApiError::Authentication(AuthError::InvalidCredentials)
        ));
    }

    #[tokio::test]
    async fn authenticate_inner_unified_api_returns_context_and_policy() {
        let app = build_test_app(Config::default()).await.expect("build app");
        let raw_key = "sk-auth-unit-test";
        let key_hash = hash_key(raw_key);
        let mut vk = sample_vk(Some(Uuid::new_v4()), None);
        vk.key_hash = key_hash;
        vk.allowed_models = Some(vec!["openai/gpt-4o-mini".to_string()]);
        vk.blocked_models = Some(vec!["openai/gpt-4o".to_string()]);

        {
            let mut cache = app.state.0.virtual_keys_cache.write().await;
            *cache = Some(rustc_hash::FxHashMap::default());
            if let Some(keys) = cache.as_mut() {
                keys.insert(vk.key_hash.clone(), vk.clone());
            }
        }

        let (auth_ctx, vk_policy) = AuthService::authenticate_request_inner(
            app.state.clone(),
            &format!("Bearer {raw_key}"),
            Some(&RequestKind::UnifiedApi),
            None,
        )
        .await
        .expect("virtual key should authenticate");

        assert_eq!(auth_ctx.org_id.as_ref(), &vk.workspace_id);
        assert_eq!(auth_ctx.master_key_id, Some(vk.master_key_id));
        assert_eq!(auth_ctx.virtual_key_id, Some(vk.id));
        assert_eq!(auth_ctx.virtual_key_prefix, vk.key_prefix);
        assert_eq!(auth_ctx.body_ttl_days, 90);

        let policy = vk_policy.expect("vk policy should be attached");
        assert_eq!(policy.virtual_key_id, vk.id);
        assert_eq!(policy.allowed_models, vk.allowed_models);
        assert_eq!(policy.blocked_models, vk.blocked_models);
    }
}
