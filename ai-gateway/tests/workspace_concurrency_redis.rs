//! Requires local Redis (default `redis://127.0.0.1:6379`), same as
//! `redis_cache` integration tests.

use std::time::Duration;

use ai_gateway::{
    app_redis::AppRedis,
    middleware::workspace_concurrency::{
        self,
        constants::{KEY_PREFIX, TTL_SECS},
    },
};
use redis::AsyncCommands;
use uuid::Uuid;

fn test_redis_url() -> url::Url {
    std::env::var("REDIS_TEST_URL")
        .unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string())
        .parse()
        .expect("REDIS_TEST_URL or default redis URL must parse")
}

#[tokio::test]
#[ignore = "Requires writable Redis (no password or set REDIS_TEST_URL); run: \
            cargo test -p ai-gateway --test workspace_concurrency_redis -- \
            --ignored"]
async fn decr_floor_never_negative() {
    let client = AppRedis::new(test_redis_url());
    let ws = Uuid::new_v4();
    let key = format!("{KEY_PREFIX}{ws}");

    let _: () = redis::cmd("DEL")
        .arg(&key)
        .query_async(&mut mq_test_conn().await)
        .await
        .expect("redis DEL");

    workspace_concurrency::redis_ops::decr_floor_refresh_ttl(&client, &key)
        .await
        .expect("decr on missing key");
    let v: Option<String> = redis::cmd("GET")
        .arg(&key)
        .query_async(&mut mq_test_conn().await)
        .await
        .expect("GET");
    assert!(
        v.is_none() || v.as_deref() == Some("0"),
        "expected absent or 0, got {v:?}"
    );

    workspace_concurrency::redis_ops::incr_refresh_ttl(&client, &key)
        .await
        .expect("incr");
    workspace_concurrency::redis_ops::incr_refresh_ttl(&client, &key)
        .await
        .expect("incr 2");
    for _ in 0..5 {
        workspace_concurrency::redis_ops::decr_floor_refresh_ttl(&client, &key)
            .await
            .expect("decr");
    }
    let v: String = redis::cmd("GET")
        .arg(&key)
        .query_async(&mut mq_test_conn().await)
        .await
        .expect("GET");
    assert_eq!(v, "0");
}

async fn mq_test_conn() -> redis::aio::MultiplexedConnection {
    let client = redis::Client::open(test_redis_url().as_str()).expect("redis client");
    client
        .get_multiplexed_async_connection()
        .await
        .expect("redis connect")
}

#[tokio::test]
#[ignore = "Requires writable Redis (no password or set REDIS_TEST_URL); run: \
            cargo test -p ai-gateway --test workspace_concurrency_redis -- \
            --ignored"]
async fn incr_and_decr_refresh_ttl() {
    let client = AppRedis::new(test_redis_url());
    let ws = Uuid::new_v4();
    let key = format!("{KEY_PREFIX}{ws}");

    let mut c = mq_test_conn().await;
    let _: () = redis::cmd("DEL")
        .arg(&key)
        .query_async(&mut c)
        .await
        .unwrap();

    workspace_concurrency::redis_ops::incr_refresh_ttl(&client, &key)
        .await
        .expect("incr");
    let ttl1: i64 = c.ttl(&key).await.expect("ttl after incr");
    assert!(
        (3500..=TTL_SECS).contains(&ttl1),
        "ttl after incr expected ~{TTL_SECS}, got {ttl1}"
    );

    tokio::time::sleep(Duration::from_secs(2)).await;

    workspace_concurrency::redis_ops::decr_floor_refresh_ttl(&client, &key)
        .await
        .expect("decr");
    let ttl2: i64 = c.ttl(&key).await.expect("ttl after decr");
    assert!(
        ttl2 > ttl1,
        "ttl should refresh on decr: ttl1={ttl1} ttl2={ttl2}"
    );
    assert!(
        (3500..=TTL_SECS).contains(&ttl2),
        "ttl after decr expected refreshed to ~{TTL_SECS}, got {ttl2}"
    );

    let _: () = redis::cmd("DEL")
        .arg(&key)
        .query_async(&mut c)
        .await
        .unwrap();
}
