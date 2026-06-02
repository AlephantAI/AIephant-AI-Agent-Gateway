use std::sync::LazyLock;

use tracing::warn;
use uuid::Uuid;

use super::constants::TTL_SECS;
use crate::app_redis::AppRedis;

static DECR_FLOOR: LazyLock<redis::Script> = LazyLock::new(|| {
    redis::Script::new(
        r"
    local n = tonumber(redis.call('GET', KEYS[1]) or '0')
    if n > 0 then
      redis.call('DECR', KEYS[1])
    end
    redis.call('EXPIRE', KEYS[1], ARGV[1])
    return redis.call('GET', KEYS[1])
",
    )
});

pub async fn incr_refresh_ttl(
    client: &AppRedis,
    key: &str,
) -> Result<(), redis::RedisError> {
    client.incr_and_expire(key, TTL_SECS).await
}

pub async fn decr_floor_refresh_ttl(
    client: &AppRedis,
    key: &str,
) -> Result<(), redis::RedisError> {
    client.invoke_script(&DECR_FLOOR, key, TTL_SECS).await
}

pub fn log_redis_err(
    op: &'static str,
    workspace: Uuid,
    err: &redis::RedisError,
) {
    warn!(
        %workspace,
        %op,
        error = %err,
        "workspace concurrency: Redis operation failed"
    );
}
