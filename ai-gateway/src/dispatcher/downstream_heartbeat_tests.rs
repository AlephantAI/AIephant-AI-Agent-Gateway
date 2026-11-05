use super::*;
use http_body_util::BodyExt;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
fn active(mode: Mode, early_sse: bool) -> Active {
    Active {
        config: HeartbeatConfig {
            interval_ms: 10,
            early_sse,
            ..HeartbeatConfig::default()
        },
        mode,
        sent: None,
    }
}
fn response(status: u16, content_type: &str, body: Body) -> Response {
    http::Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .body(body)
        .unwrap()
}
#[tokio::test]
async fn bounded_frames_preserve_split_lines_and_reject_incomplete_or_oversize_frames() {
    for pieces in [
        vec!["data: x\r", "\n\r", "\ndata: y\n\n"],
        vec!["data: x\r", "\rdata: y\r\r"],
        vec!["data: x\n", "\ndata: y\n\n"],
    ] {
        let source = Box::pin(futures::stream::iter(
            pieces.into_iter().map(|s| Ok(Bytes::from(s))),
        ));
        let got: Vec<_> = frames(source, 64).collect().await;
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got.iter().all(Result::is_ok), "{got:?}");
    }
    let mut large = frames(
        Box::pin(futures::stream::iter([Ok(Bytes::from(vec![b'x'; 65]))])),
        64,
    );
    assert!(large.next().await.unwrap().is_err());
    let mut incomplete = frames(
        Box::pin(futures::stream::iter([Ok(Bytes::from_static(
            b"data: unfinished",
        ))])),
        64,
    );
    assert!(incomplete.next().await.unwrap().is_err());
}
#[tokio::test]
async fn json_keeps_beating_while_upstream_body_is_pending_after_headers() {
    let source = futures::stream::once(async {
        tokio::time::sleep(Duration::from_millis(80)).await;
        Ok::<_, Infallible>(Bytes::from_static(b"{\"ok\":true}"))
    });
    let future =
        Box::pin(async { Ok(response(200, "application/json", Body::from_stream(source))) });
    let mut body = respond(future, active(Mode::Json, false))
        .await
        .unwrap()
        .into_body();
    let first = tokio::time::timeout(Duration::from_millis(60), body.frame())
        .await
        .expect("heartbeat before upstream body")
        .unwrap()
        .unwrap()
        .into_data()
        .unwrap();
    assert_eq!(first, b"\n".as_slice());
    let rest = body.collect().await.unwrap().to_bytes();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&rest).unwrap()["ok"],
        true
    );
}
#[tokio::test]
async fn established_sse_heartbeats_never_split_business_frames_and_emit_one_terminal_error() {
    let source = futures::stream::unfold(0, |n| async move {
        match n {
            0 => Some((
                Ok::<_, ApiError>(Bytes::from_static(b"data: {\"value\":")),
                1,
            )),
            1 => {
                tokio::time::sleep(Duration::from_millis(45)).await;
                Some((Ok(Bytes::from_static(b"1}\n\n")), 2))
            }
            2 => {
                tokio::time::sleep(Duration::from_millis(30)).await;
                Some((Err(fail("upstream_disconnected")), 3))
            }
            _ => None,
        }
    });
    let body = established(
        response(200, "text/event-stream", Body::from_stream(source)),
        &active(Mode::Sse, false),
    )
    .into_body()
    .collect()
    .await
    .unwrap()
    .to_bytes();
    let text = std::str::from_utf8(&body).unwrap();
    assert!(text.starts_with(": alephant heartbeat\n\n"));
    assert!(text.contains("data: {\"value\":1}\n\n"), "{text}");
    assert_eq!(text.matches("data: [DONE]").count(), 1);
    assert!(text.contains("\"error\""));
}
#[tokio::test]
async fn delayed_failures_are_explicit_after_commit_and_uncommitted_errors_retain_status() {
    let future = Box::pin(async {
        tokio::time::sleep(Duration::from_millis(40)).await;
        Ok(response(429, "application/json", Body::from("private")))
    });
    let result = respond(future, active(Mode::Json, false)).await.unwrap();
    assert_eq!(result.status(), 200);
    let json: serde_json::Value =
        serde_json::from_slice(&result.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(json["upstream_status"], 429);
    assert!(json.get("error").is_some());
    assert!(!json.to_string().contains("private"));
    let future = Box::pin(async { Ok(response(429, "application/json", Body::from("original"))) });
    assert_eq!(
        respond(future, active(Mode::Sse, false))
            .await
            .unwrap()
            .status(),
        429
    );
}
struct DropSignal(Arc<AtomicBool>);
impl Drop for DropSignal {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}
#[tokio::test]
async fn disconnect_drops_pending_operation_and_slow_consumers_do_not_prefetch() {
    let dropped = Arc::new(AtomicBool::new(false));
    let signal = DropSignal(dropped.clone());
    let future = Box::pin(async move {
        let _signal = signal;
        futures::future::pending::<()>().await;
        Ok(response(200, "application/json", Body::empty()))
    });
    let mut body = respond(future, active(Mode::Json, false))
        .await
        .unwrap()
        .into_body();
    body.frame().await.unwrap().unwrap();
    drop(body);
    assert!(dropped.load(Ordering::SeqCst));
    let polled = Arc::new(AtomicUsize::new(0));
    let count = polled.clone();
    let source = futures::stream::iter((0..100).map(move |_| {
        count.fetch_add(1, Ordering::SeqCst);
        Ok(Bytes::from_static(b"data: x\n\n"))
    }));
    let mut stream = frames(Box::pin(source), 64);
    stream.next().await.unwrap().unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(polled.load(Ordering::SeqCst), 1);
}
#[test]
fn configuration_excludes_binary_and_rejects_bad_bounds() {
    let config = HeartbeatConfig::default();
    config.validate().unwrap();
    assert!(select(Some(&config), "audio/speech", br#"{"stream":true}"#).is_none());
    assert!(select(None, "chat/completions", br#"{"stream":true}"#).is_none());
    assert!(
        HeartbeatConfig {
            interval_ms: 0,
            ..config.clone()
        }
        .validate()
        .is_err()
    );
    assert!(
        HeartbeatConfig {
            paths: vec!["audio/speech".into()],
            ..config
        }
        .validate()
        .is_err()
    );
}

#[tokio::test]
async fn bounded_upstream_requires_terminal_event_without_reconnecting() {
    use crate::agent::tools::mcp_streamable_http::test_support::{
        StreamableFixture, sse_raw_response,
    };
    let fixture = StreamableFixture::start(vec![sse_raw_response(
        "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n",
    )])
    .expect("loopback fixture");
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let result: Vec<_> = upstream_sse(client.post(fixture.url()), "{}", 1024)
        .await
        .unwrap()
        .collect()
        .await;
    assert!(
        result.last().unwrap().is_err(),
        "missing terminal event is an uncertain outcome"
    );
    assert_eq!(fixture.requests().len(), 1, "must not reconnect or replay");
}
