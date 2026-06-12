use std::{
    collections::HashMap,
    future::Future,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::{agent::tools::executor::ToolExecutionContext, app_redis::AppRedis};

pub const CLIENT_PROTOCOL_FAMILY: &str = "mcp-2025-06-18-compatible";
const SESSION_LOCK_TIMEOUT_BUFFER_SECS: u64 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct McpStreamableSession {
    pub session_id: String,
    pub negotiated_protocol_version: String,
    pub target_hash: String,
    pub auth_revision: String,
    pub server_info: serde_json::Value,
    pub capabilities: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub last_used_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[async_trait]
pub trait McpSessionCache: Send + Sync {
    async fn load(&self, key: &str) -> Option<McpStreamableSession>;

    async fn store(&self, key: &str, value: &McpStreamableSession, ttl_secs: u64);

    async fn delete(&self, key: &str);
}

#[derive(Debug, Clone)]
pub struct RedisMcpSessionCache {
    redis: Arc<AppRedis>,
}

impl RedisMcpSessionCache {
    pub fn new(redis: Arc<AppRedis>) -> Self {
        Self { redis }
    }
}

#[async_trait]
impl McpSessionCache for RedisMcpSessionCache {
    async fn load(&self, key: &str) -> Option<McpStreamableSession> {
        let Ok(Some(value)) = self.redis.get_string(key).await else {
            return None;
        };
        serde_json::from_str(&value).ok()
    }

    async fn store(&self, key: &str, session: &McpStreamableSession, ttl_secs: u64) {
        let Ok(value) = serde_json::to_string(session) else {
            return;
        };
        let _ = self.redis.set_ex(key, &value, ttl_secs).await;
    }

    async fn delete(&self, key: &str) {
        let _ = self.redis.del(key).await;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionLockToken {
    key: String,
    value: String,
}

impl SessionLockToken {
    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn value(&self) -> &str {
        &self.value
    }
}

pub async fn try_acquire_session_lock(
    ctx: &ToolExecutionContext,
) -> Result<Option<SessionLockToken>, redis::RedisError> {
    try_acquire_session_lock_with_ttl(ctx, ctx.mcp_session_lock_ttl_secs).await
}

pub async fn try_acquire_session_lock_with_ttl(
    ctx: &ToolExecutionContext,
    ttl_secs: u64,
) -> Result<Option<SessionLockToken>, redis::RedisError> {
    let Some(redis) = &ctx.redis else {
        return Ok(Some(SessionLockToken {
            key: session_lock_key(ctx),
            value: "local-no-redis".to_string(),
        }));
    };
    let key = session_lock_key(ctx);
    let value = uuid::Uuid::now_v7().to_string();
    let acquired = redis.set_nx_ex(&key, &value, ttl_secs.max(1)).await?;
    Ok(acquired.then_some(SessionLockToken { key, value }))
}

pub async fn release_session_lock(
    ctx: &ToolExecutionContext,
    token: &SessionLockToken,
) -> Result<bool, redis::RedisError> {
    let Some(redis) = &ctx.redis else {
        return Ok(true);
    };
    redis.del_if_value(&token.key, &token.value).await
}

pub fn should_evict_session(ctx: &ToolExecutionContext, session: &McpStreamableSession) -> bool {
    session.target_hash != ctx.target_hash || session.auth_revision != ctx.auth_revision
}

pub fn session_lock_ttl_secs_for_timeout(ctx: &ToolExecutionContext, timeout_ms: u64) -> u64 {
    let timeout_secs = (timeout_ms / 1000).saturating_add(u64::from(timeout_ms % 1000 != 0));
    ctx.mcp_session_lock_ttl_secs
        .max(timeout_secs.saturating_add(SESSION_LOCK_TIMEOUT_BUFFER_SECS))
}

pub struct InMemorySessionSingleflight<T = McpStreamableSession> {
    in_flight: Arc<Mutex<HashMap<String, broadcast::Sender<Option<T>>>>>,
}

impl<T> Clone for InMemorySessionSingleflight<T> {
    fn clone(&self) -> Self {
        Self {
            in_flight: self.in_flight.clone(),
        }
    }
}

impl<T> Default for InMemorySessionSingleflight<T> {
    fn default() -> Self {
        Self {
            in_flight: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl<T> InMemorySessionSingleflight<T>
where
    T: Clone + Send + 'static,
{
    pub async fn get_or_init<F, Fut>(&self, key: String, init: F) -> T
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = T>,
    {
        self.get_or_try_init(key, || async move {
            Ok::<Option<T>, std::convert::Infallible>(Some(init().await))
        })
        .await
        .expect("infallible singleflight initializer")
        .expect("infallible singleflight initializer returns session")
    }

    pub async fn get_or_try_init<F, Fut, E>(&self, key: String, init: F) -> Result<Option<T>, E>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Option<T>, E>>,
    {
        let mut init = Some(init);
        loop {
            let role = {
                let mut guard = self.in_flight.lock().expect("singleflight lock");
                if let Some(sender) = guard.get(&key) {
                    SingleflightRole::Wait(sender.subscribe())
                } else {
                    let (sender, _) = broadcast::channel(1);
                    guard.insert(key.clone(), sender.clone());
                    SingleflightRole::Owner(SingleflightOwner::new(
                        key.clone(),
                        self.in_flight.clone(),
                        sender,
                    ))
                }
            };

            match role {
                SingleflightRole::Owner(owner) => {
                    let init = init.take().expect("singleflight initializer used once");
                    let session = init().await?;
                    owner.finish(session.clone());
                    return Ok(session);
                }
                SingleflightRole::Wait(mut receiver) => match receiver.recv().await {
                    Ok(session) => return Ok(session),
                    Err(broadcast::error::RecvError::Closed) => continue,
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        continue;
                    }
                },
            }
        }
    }
}

enum SingleflightRole<T> {
    Owner(SingleflightOwner<T>),
    Wait(broadcast::Receiver<Option<T>>),
}

struct SingleflightOwner<T> {
    key: String,
    in_flight: Arc<Mutex<HashMap<String, broadcast::Sender<Option<T>>>>>,
    sender: Option<broadcast::Sender<Option<T>>>,
}

impl<T> SingleflightOwner<T>
where
    T: Clone,
{
    fn new(
        key: String,
        in_flight: Arc<Mutex<HashMap<String, broadcast::Sender<Option<T>>>>>,
        sender: broadcast::Sender<Option<T>>,
    ) -> Self {
        Self {
            key,
            in_flight,
            sender: Some(sender),
        }
    }

    fn finish(mut self, session: Option<T>) {
        if let Some(sender) = self.sender.take() {
            let mut guard = self.in_flight.lock().expect("singleflight lock");
            guard.remove(&self.key);
            drop(guard);
            let _ = sender.send(session.clone());
        }
    }
}

impl<T> Drop for SingleflightOwner<T> {
    fn drop(&mut self) {
        if self.sender.is_some() {
            let mut guard = self.in_flight.lock().expect("singleflight lock");
            guard.remove(&self.key);
        }
    }
}

pub fn session_key(ctx: &ToolExecutionContext) -> String {
    format!(
        "agent:mcp:streamable-http:session:workspace:{}:vk:{}:agent:{}:caller:\
         {}:target:{}:target-rev:{}:target-hash:{}:auth-rev:{}:protocol:{}",
        ctx.workspace_id,
        ctx.virtual_key_id.as_deref().unwrap_or("no-vk"),
        ctx.agent_id,
        ctx.caller_principal_id,
        ctx.target_id,
        ctx.target_revision,
        ctx.target_hash,
        ctx.auth_revision,
        CLIENT_PROTOCOL_FAMILY
    )
}

pub fn session_lock_key(ctx: &ToolExecutionContext) -> String {
    format!(
        "agent:mcp:streamable-http:session-lock:{}",
        session_key(ctx)
    )
}

pub fn validate_session_id(session_id: &str) -> Result<(), &'static str> {
    if session_id.is_empty() {
        return Err("session id must not be empty");
    }
    if session_id.len() > 256 {
        return Err("session id must be at most 256 bytes");
    }
    if !session_id.bytes().all(|byte| (0x21..=0x7e).contains(&byte)) {
        return Err("session id must contain only visible ASCII bytes");
    }
    Ok(())
}

#[derive(Clone, Default)]
pub struct NoopMcpSessionCache;

#[async_trait]
impl McpSessionCache for NoopMcpSessionCache {
    async fn load(&self, _key: &str) -> Option<McpStreamableSession> {
        None
    }

    async fn store(&self, _key: &str, _value: &McpStreamableSession, _ttl_secs: u64) {}

    async fn delete(&self, _key: &str) {}
}

#[cfg(test)]
#[derive(Clone, Default)]
pub struct InMemoryMcpSessionCache {
    values: Arc<Mutex<HashMap<String, McpStreamableSession>>>,
}

#[cfg(test)]
#[async_trait]
impl McpSessionCache for InMemoryMcpSessionCache {
    async fn load(&self, key: &str) -> Option<McpStreamableSession> {
        self.values
            .lock()
            .expect("session cache lock")
            .get(key)
            .cloned()
    }

    async fn store(&self, key: &str, value: &McpStreamableSession, _ttl_secs: u64) {
        self.values
            .lock()
            .expect("session cache lock")
            .insert(key.to_string(), value.clone());
    }

    async fn delete(&self, key: &str) {
        self.values.lock().expect("session cache lock").remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_key_uses_isolated_billing_and_auth_dimensions() {
        let ctx = context(Some("vk-1"));

        let key = session_key(&ctx);

        assert_eq!(
            key,
            concat!(
                "agent:mcp:streamable-http:session:",
                "workspace:workspace-a:",
                "vk:vk-1:",
                "agent:agent-a:",
                "caller:caller-a:",
                "target:target-a:",
                "target-rev:17:",
                "target-hash:sha256:target-a:",
                "auth-rev:auth-a:",
                "protocol:mcp-2025-06-18-compatible"
            )
        );
        assert_eq!(
            session_lock_key(&ctx),
            format!("agent:mcp:streamable-http:session-lock:{key}")
        );

        let without_vk = context(None);
        assert!(session_key(&without_vk).contains("no-vk"));
        assert_ne!(key, session_key(&without_vk));
    }

    #[test]
    fn session_id_rejects_unsafe_values() {
        assert!(validate_session_id("session-1").is_ok());
        assert!(validate_session_id("!~").is_ok());
        assert!(validate_session_id("").is_err());
        assert!(validate_session_id(&"a".repeat(257)).is_err());
        assert!(validate_session_id("has space").is_err());
        assert!(validate_session_id("has\nnewline").is_err());
        assert!(validate_session_id("snowman-\u{2603}").is_err());
    }

    #[test]
    fn session_value_target_hash_mismatch_is_not_reused() {
        let ctx = context(Some("vk-1"));
        let session = McpStreamableSession {
            target_hash: "sha256:old-target".to_string(),
            ..session("session-1")
        };

        assert!(should_evict_session(&ctx, &session));
    }

    #[test]
    fn session_value_actual_negotiated_version_is_reused() {
        let ctx = context(Some("vk-1"));
        let session = McpStreamableSession {
            negotiated_protocol_version: "2025-06-18".to_string(),
            ..session("session-1")
        };

        assert!(!should_evict_session(&ctx, &session));
    }

    #[tokio::test]
    async fn in_memory_session_cache_matches_key_based_trait_shape() {
        let cache = InMemoryMcpSessionCache::default();
        let key = "session-key";
        let session = session("session-1");

        assert!(cache.load(key).await.is_none());
        cache.store(key, &session, 60).await;
        assert_eq!(cache.load(key).await, Some(session));
        cache.delete(key).await;
        assert!(cache.load(key).await.is_none());
    }

    #[test]
    fn session_lock_ttl_covers_lifecycle_timeout_with_buffer() {
        let mut ctx = context(Some("vk-1"));
        ctx.mcp_session_lock_ttl_secs = 5;

        assert_eq!(session_lock_ttl_secs_for_timeout(&ctx, 1000), 5);
        assert_eq!(session_lock_ttl_secs_for_timeout(&ctx, 4001), 6);
        assert_eq!(session_lock_ttl_secs_for_timeout(&ctx, 8000), 9);
    }

    #[test]
    fn session_lock_ttl_ceil_does_not_overflow_for_large_timeout() {
        let mut ctx = context(Some("vk-1"));
        ctx.mcp_session_lock_ttl_secs = 5;

        assert_eq!(
            session_lock_ttl_secs_for_timeout(&ctx, u64::MAX),
            (u64::MAX / 1000) + 2
        );
    }

    #[test]
    fn session_value_roundtrips_without_exposing_secret_fields() {
        let now = Utc::now();
        let session = McpStreamableSession {
            session_id: "session-1".to_string(),
            negotiated_protocol_version: "2025-06-18".to_string(),
            target_hash: "sha256:test".to_string(),
            auth_revision: "0/static".to_string(),
            server_info: serde_json::json!({"name": "test"}),
            capabilities: serde_json::json!({"tools": {}}),
            created_at: now,
            last_used_at: now,
            expires_at: now,
        };

        let raw = serde_json::to_string(&session).expect("session JSON");

        assert!(raw.contains("sessionId"));
        assert!(!raw.contains("Authorization"));
        assert!(!raw.contains("Mcp-Session-Id"));
        let decoded: McpStreamableSession = serde_json::from_str(&raw).expect("decode session");
        assert_eq!(decoded.session_id, "session-1");
    }

    #[tokio::test]
    async fn concurrent_cache_miss_uses_one_initializer_per_key() {
        let singleflight = InMemorySessionSingleflight::default();
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let mut handles = Vec::new();
        for _ in 0..8 {
            let singleflight = singleflight.clone();
            let calls = calls.clone();
            handles.push(tokio::spawn(async move {
                singleflight
                    .get_or_init("key-a".to_string(), || {
                        let calls = calls.clone();
                        async move {
                            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                            session("session-1")
                        }
                    })
                    .await
            }));
        }

        let sessions = futures::future::join_all(handles).await;

        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        for session in sessions {
            assert_eq!(session.expect("task joins").session_id, "session-1");
        }
    }

    #[tokio::test]
    async fn initializer_panic_cleans_up_singleflight_key() {
        let singleflight = InMemorySessionSingleflight::default();
        let panicking = singleflight.clone();

        let result = tokio::spawn(async move {
            panicking
                .get_or_init("key-a".to_string(), || async {
                    panic!("initializer failed")
                })
                .await
        })
        .await;
        assert!(result.expect_err("initializer task panics").is_panic());

        let session = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            singleflight.get_or_init("key-a".to_string(), || async {
                session("session-after-panic")
            }),
        )
        .await
        .expect("singleflight key is released after panic");

        assert_eq!(session.session_id, "session-after-panic");
    }

    fn context(virtual_key_id: Option<&str>) -> ToolExecutionContext {
        ToolExecutionContext {
            workspace_id: "workspace-a".to_string(),
            virtual_key_id: virtual_key_id.map(str::to_string),
            agent_id: "agent-a".to_string(),
            caller_principal_id: "caller-a".to_string(),
            target_id: "target-a".to_string(),
            target_revision: 17,
            target_hash: "sha256:target-a".to_string(),
            auth_revision: "auth-a".to_string(),
            redis: None,
            mcp_session_cache_ttl_secs: 60,
            mcp_session_lock_ttl_secs: 5,
            mcp_session_max_concurrent_per_session: 1,
            mcp_sse_max_event_bytes: 1024,
            mcp_sse_max_line_bytes: 1024,
            mcp_sse_max_events: 10,
            mcp_sse_max_batch_items: 10,
            mcp_sse_idle_timeout_ms: 1000,
        }
    }

    fn session(session_id: &str) -> McpStreamableSession {
        let now = Utc::now();
        McpStreamableSession {
            session_id: session_id.to_string(),
            negotiated_protocol_version: CLIENT_PROTOCOL_FAMILY.to_string(),
            target_hash: "sha256:target-a".to_string(),
            auth_revision: "auth-a".to_string(),
            server_info: serde_json::json!({ "name": "fixture" }),
            capabilities: serde_json::json!({}),
            created_at: now,
            last_used_at: now,
            expires_at: now + chrono::Duration::seconds(60),
        }
    }
}
