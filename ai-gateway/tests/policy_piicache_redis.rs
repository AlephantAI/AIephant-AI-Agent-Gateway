//! Requires local Redis (default `redis://127.0.0.1:6379`).

use std::sync::Arc;

use ai_gateway::{
    app_redis::AppRedis,
    content_filter::piicache::{
        attach_request_body_from_piicache_redis, piicache_redis_key,
    },
    metrics::VkMetrics,
    policy_proto::EvaluateRequest,
};
use bytes::Bytes;
use opentelemetry::global;
use uuid::Uuid;

fn test_redis_url() -> url::Url {
    std::env::var("REDIS_TEST_URL")
        .unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string())
        .parse()
        .expect("REDIS_TEST_URL or default redis URL must parse")
}

async fn test_conn() -> redis::aio::MultiplexedConnection {
    let client =
        redis::Client::open(test_redis_url().as_str()).expect("redis client");
    client
        .get_multiplexed_async_connection()
        .await
        .expect("redis connect")
}

#[tokio::test]
#[ignore = "Requires writable Redis; run: cargo test -p ai-gateway --features \
            external --test policy_piicache_redis -- --ignored"]
async fn attach_request_body_when_piicache_true() {
    let ws = Uuid::new_v4().to_string();
    let key = piicache_redis_key(&ws);
    let mut conn = test_conn().await;
    let _: () = redis::cmd("DEL")
        .arg(&key)
        .query_async(&mut conn)
        .await
        .expect("redis DEL");
    let _: () = redis::cmd("SET")
        .arg(&key)
        .arg("true")
        .query_async(&mut conn)
        .await
        .expect("redis SET");

    let redis = Arc::new(AppRedis::new(test_redis_url()));
    let meter = global::meter("policy_piicache_test");
    let vk = VkMetrics::new(&meter);
    let body = Bytes::from_static(b"{\"model\":\"x\"}");
    let mut req = EvaluateRequest {
        workspace_id: ws.clone(),
        ..Default::default()
    };

    attach_request_body_from_piicache_redis(
        Some(&redis),
        &ws,
        &body,
        &mut req,
        &vk,
    )
    .await;

    assert_eq!(req.body, body.to_vec());

    let _: () = redis::cmd("DEL")
        .arg(&key)
        .query_async(&mut conn)
        .await
        .expect("redis DEL");
}
