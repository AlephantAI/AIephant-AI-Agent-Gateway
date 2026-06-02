use std::sync::LazyLock;

use redis::Script;

use crate::app_redis::AppRedis;

pub const TTL_SECS: i64 = 3600;

static DECR_FLOOR: LazyLock<Script> = LazyLock::new(|| {
    Script::new(
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

pub async fn decr_floor_refresh_ttl(
    client: &AppRedis,
    key: &str,
) -> Result<(), redis::RedisError> {
    client.invoke_script(&DECR_FLOOR, key, TTL_SECS).await
}
