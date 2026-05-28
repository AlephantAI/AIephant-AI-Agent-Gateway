use http::StatusCode;
use uuid::Uuid;

use crate::app_state::AppState;

pub const REDIS_TTL_SECS: u64 = 7 * 24 * 60 * 60;
pub const FALLBACK_CACHE_MAX_CAPACITY: u64 = 10_000;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum EndpointRegion {
    Cn,
}

impl EndpointRegion {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cn => "cn",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "cn" => Some(Self::Cn),
            _ => None,
        }
    }
}

pub type RegionalEndpointCache = moka::future::Cache<Uuid, EndpointRegion>;

#[must_use]
pub fn new_fallback_cache() -> RegionalEndpointCache {
    RegionalEndpointCache::builder()
        .max_capacity(FALLBACK_CACHE_MAX_CAPACITY)
        .time_to_live(std::time::Duration::from_secs(REDIS_TTL_SECS))
        .build()
}

#[must_use]
// Used by Task 3 dispatcher retry wiring; tests cover it in Task 2.
#[allow(dead_code)]
pub fn redis_key(master_key_id: Uuid) -> String {
    format!("regional_endpoint:{master_key_id}")
}

#[must_use]
// Used by Task 3 dispatcher retry wiring; tests cover it in Task 2.
#[allow(dead_code)]
pub fn regional_retry_eligible_status(status: StatusCode) -> bool {
    matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN)
}

// Used by Task 3 dispatcher retry wiring.
#[allow(dead_code)]
pub async fn get_learned_region(
    app_state: &AppState,
    master_key_id: Option<Uuid>,
) -> Option<EndpointRegion> {
    let master_key_id = master_key_id?;
    let key = redis_key(master_key_id);

    if let Some(redis) = app_state.redis() {
        match redis.get_string(&key).await {
            Ok(Some(value)) => {
                if let Some(region) = EndpointRegion::parse(&value) {
                    return Some(region);
                }
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    master_key_id = %master_key_id,
                    "regional_endpoint: redis get failed"
                );
            }
        }
    }

    app_state
        .0
        .regional_endpoint_cache
        .get(&master_key_id)
        .await
}

// Used by Task 3 dispatcher retry wiring.
#[allow(dead_code)]
pub async fn remember_region(
    app_state: &AppState,
    master_key_id: Option<Uuid>,
    region: EndpointRegion,
) {
    let Some(master_key_id) = master_key_id else {
        return;
    };
    let key = redis_key(master_key_id);

    if let Some(redis) = app_state.redis() {
        match redis.set_ex(&key, region.as_str(), REDIS_TTL_SECS).await {
            Ok(()) => {}
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    master_key_id = %master_key_id,
                    "regional_endpoint: redis set_ex failed"
                );
            }
        }
    }

    app_state
        .0
        .regional_endpoint_cache
        .insert(master_key_id, region)
        .await;
}

// Used by Task 3 dispatcher retry wiring.
#[allow(dead_code)]
pub async fn forget_region(app_state: &AppState, master_key_id: Option<Uuid>) {
    let Some(master_key_id) = master_key_id else {
        return;
    };
    let key = redis_key(master_key_id);

    if let Some(redis) = app_state.redis() {
        if let Err(error) = redis.del(&key).await {
            tracing::warn!(
                error = %error,
                master_key_id = %master_key_id,
                "regional_endpoint: redis delete failed"
            );
        }
    }

    app_state
        .0
        .regional_endpoint_cache
        .invalidate(&master_key_id)
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redis_key_includes_master_key_id() {
        let master_key_id = Uuid::parse_str("67e55044-10b1-426f-9247-bb680e5fe0c8").unwrap();

        assert_eq!(
            redis_key(master_key_id),
            "regional_endpoint:67e55044-10b1-426f-9247-bb680e5fe0c8"
        );
    }

    #[test]
    fn parses_known_region_only() {
        assert_eq!(EndpointRegion::parse("cn"), Some(EndpointRegion::Cn));
        assert_eq!(EndpointRegion::parse("CN"), None);
        assert_eq!(EndpointRegion::parse("us"), None);
        assert_eq!(EndpointRegion::parse(""), None);
    }

    #[test]
    fn eligible_statuses_are_conservative() {
        assert!(regional_retry_eligible_status(StatusCode::UNAUTHORIZED));
        assert!(regional_retry_eligible_status(StatusCode::FORBIDDEN));
        assert!(!regional_retry_eligible_status(
            StatusCode::TOO_MANY_REQUESTS
        ));
        assert!(!regional_retry_eligible_status(StatusCode::NOT_FOUND));
        assert!(!regional_retry_eligible_status(
            StatusCode::INTERNAL_SERVER_ERROR
        ));
    }

    #[tokio::test]
    async fn forget_region_without_master_key_is_noop() {
        let app = crate::app::build_test_app(crate::config::Config::default())
            .await
            .expect("build test app");

        forget_region(&app.state, None).await;
    }

    #[tokio::test]
    async fn remembers_reads_and_forgets_with_redis_disabled() {
        let app = crate::app::build_test_app(crate::config::Config::default())
            .await
            .expect("build test app");
        let master_key_id = Uuid::new_v4();

        remember_region(&app.state, Some(master_key_id), EndpointRegion::Cn).await;
        assert_eq!(
            get_learned_region(&app.state, Some(master_key_id)).await,
            Some(EndpointRegion::Cn)
        );

        forget_region(&app.state, Some(master_key_id)).await;

        assert_eq!(
            get_learned_region(&app.state, Some(master_key_id)).await,
            None
        );
    }
}
