use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use alephant_llm_kv_cache::{
    LazyCloudflareKvBackend, LlmKvBackend, cloudflare::CloudflareKvClient,
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

#[tokio::test]
async fn get_uses_custom_api_base() {
    let srv = MockServer::start().await;
    let key = "abc123";
    Mock::given(method("GET"))
        .and(path(format!(
            "/custom/prefix/accounts/acct/storage/kv/namespaces/ns/values/\
             {key}"
        )))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(r#"{"headers":{},"latency":1,"body":["x"]}"#),
        )
        .mount(&srv)
        .await;

    let c = CloudflareKvClient {
        http: reqwest::Client::new(),
        api_base: format!("{}/custom/prefix", srv.uri()),
        account_id: "acct".into(),
        namespace_id: "ns".into(),
        api_token: "tok".into(),
    };
    let v = c.get_raw(key).await.unwrap();
    assert!(v.is_some());
}

#[tokio::test]
async fn lazy_cf_second_get_during_cooling_skips_http() {
    let srv = MockServer::start().await;
    let key = "cool";
    let p = format!("/accounts/acct/storage/kv/namespaces/ns/values/{key}");
    Mock::given(method("GET"))
        .and(path(p.clone()))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount(&srv)
        .await;

    let inner = CloudflareKvClient {
        http: reqwest::Client::new(),
        api_base: srv.uri().to_string(),
        account_id: "acct".into(),
        namespace_id: "ns".into(),
        api_token: "tok".into(),
    };
    let lazy = LazyCloudflareKvBackend::new(inner);

    assert!(lazy.get(key).await.unwrap().is_none());
    assert!(lazy.get(key).await.unwrap().is_none());
}

#[tokio::test]
async fn lazy_cf_503_then_retry_gets_value() {
    let srv = MockServer::start().await;
    let key = "retry";
    let p = format!("/accounts/acct/storage/kv/namespaces/ns/values/{key}");

    let call = Arc::new(AtomicUsize::new(0));
    let call_fn = Arc::clone(&call);
    Mock::given(method("GET"))
        .and(path(p.clone()))
        .respond_with(move |_req: &wiremock::Request| {
            let i = call_fn.fetch_add(1, Ordering::SeqCst);
            if i == 0 {
                ResponseTemplate::new(503)
            } else {
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"headers":{},"latency":1,"body":["x"]}"#)
            }
        })
        .mount(&srv)
        .await;

    let inner = CloudflareKvClient {
        http: reqwest::Client::new(),
        api_base: srv.uri().to_string(),
        account_id: "acct".into(),
        namespace_id: "ns".into(),
        api_token: "tok".into(),
    };
    let lazy = LazyCloudflareKvBackend::new(inner);

    assert!(lazy.get(key).await.unwrap().is_none());

    // First failure uses failures=1 → 400ms backoff; wait past that window.
    tokio::time::sleep(Duration::from_millis(600)).await;

    let v = lazy.get(key).await.unwrap();
    assert!(v.is_some());
    assert_eq!(call.load(Ordering::SeqCst), 2);
}
