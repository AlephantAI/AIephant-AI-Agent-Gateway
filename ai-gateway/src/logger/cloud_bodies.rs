use bytes::Bytes;
use uuid::Uuid;

use crate::{
    error::logger::LoggerError,
    logger::body_storage::{
        LARGE_BODY_THRESHOLD_BYTES, clamp_body_ttl_days, inline_body_for_log,
        storage_location_for_sizes,
    },
    store::s3::BaseS3Client,
    types::org::OrgId,
};

/// Cloud-only: when **both** bodies are below 1 MiB, inline UTF-8 strings for
/// `ClickHouse`. When **either** body is at least 1 MiB, **`storage_location`
/// is `s3`** and **both** sides are stored as objects (PUT + presigned GET
/// URL), including the side that is still under 1 MiB.
///
/// Exposed for manual smoke tests (see crate example
/// `cloud_log_bodies_real_storage`); production code should use
/// [`crate::logger::service::LoggerService::log`].
pub async fn resolve_cloud_log_bodies(
    s3: &BaseS3Client,
    body_ttl_days: u16,
    request_id: Uuid,
    org_id: OrgId,
    request_body: &Bytes,
    response_body: &Bytes,
) -> Result<(String, String, u16, String), LoggerError> {
    let ttl = clamp_body_ttl_days(body_ttl_days);
    let storage = storage_location_for_sizes(request_body.len(), response_body.len()).to_string();
    let threshold = LARGE_BODY_THRESHOLD_BYTES;
    let store_both_on_s3 = request_body.len() >= threshold || response_body.len() >= threshold;

    let request_body_str = if store_both_on_s3 {
        let key = format!(
            "organizations/{org_id}/ttl-{ttl}/requests/{request_id}/\
             request_body",
        );
        let put_url = s3.sign_put_url_for_object(&key);
        let url = s3
            .put_log_body_object_and_presign_get(
                put_url,
                &key,
                request_body.clone(),
                "application/octet-stream",
            )
            .await?;
        url.to_string()
    } else {
        inline_body_for_log(request_body)
    };

    let response_body_str = if store_both_on_s3 {
        let key = format!(
            "organizations/{org_id}/ttl-{ttl}/requests/{request_id}/\
             response_body",
        );
        let put_url = s3.sign_put_url_for_object(&key);
        let url = s3
            .put_log_body_object_and_presign_get(
                put_url,
                &key,
                response_body.clone(),
                "application/octet-stream",
            )
            .await?;
        url.to_string()
    } else {
        inline_body_for_log(response_body)
    };

    Ok((request_body_str, response_body_str, ttl, storage))
}

#[cfg(all(test, feature = "testing"))]
mod tests {
    use std::collections::{HashMap, HashSet};

    use stubr::wiremock_rs::Times;
    use tracing::info;
    use url::Url;
    use uuid::Uuid;

    use super::*;
    use crate::{config::s3::Config, tests::TestDefault};

    fn s3_stub_path() -> &'static str {
        concat!(env!("CARGO_MANIFEST_DIR"), "/stubs/s3")
    }

    async fn start_s3_mock(put_expectation: Times) -> (stubr::Stubr, BaseS3Client) {
        let stubs = HashMap::from([("success:s3:upload_log_body_cloud", put_expectation)]);
        let active = HashSet::from(["success:s3:upload_log_body_cloud"]);
        let mock = stubr::Stubr::try_start_with(
            s3_stub_path(),
            Some(active),
            stubr::Config {
                record: true,
                global_delay: None,
                verify: true,
                port: None,
                ..Default::default()
            },
        )
        .await
        .expect("s3 stub");
        for (name, times) in &stubs {
            mock.http_server.set_expectation(name, times.clone()).await;
        }
        let mut cfg = Config::test_default();
        cfg.endpoint = Url::parse(&mock.uri()).expect("mock url");
        let client = BaseS3Client::new(cfg).expect("s3 client");
        (mock, client)
    }

    /// Log a synthetic `/v1/log/request` payload summary (field shapes only).
    fn info_log_request_summary(
        scenario: &str,
        request_body_str: &str,
        response_body_str: &str,
        body_ttl_days: u16,
        storage_location: &str,
    ) {
        let summarize = |s: &str| -> serde_json::Value {
            if s.starts_with("http://") || s.starts_with("https://") {
                serde_json::json!({
                    "kind": "presigned_get_url",
                    "byte_len": s.len(),
                    "url_prefix": s.chars().take(120).collect::<String>(),
                })
            } else {
                serde_json::json!({
                    "kind": "inline_utf8",
                    "byte_len": s.len(),
                    "preview": s.chars().take(200).collect::<String>(),
                })
            }
        };
        let payload = serde_json::json!({
            "scenario": scenario,
            "storageLocation": storage_location,
            "log.request": {
                "bodyTtlDays": body_ttl_days,
                "body": summarize(request_body_str),
                "note": "Real POST includes log.request.storageLocation; this block is shape-only",
            },
            "log.response": {
                "body": summarize(response_body_str),
            }
        });
        info!(
            "\n=== cloud logger /v1/log/request field summary ===\n{}\n",
            serde_json::to_string_pretty(&payload).expect("json")
        );
    }

    #[tokio::test]
    async fn cloud_request_over_1_mib_yields_s3_and_presigned_request_field() {
        let threshold = LARGE_BODY_THRESHOLD_BYTES;
        let (_mock, s3) = start_s3_mock(2.into()).await;
        let rid = Uuid::nil();
        let org = OrgId::new(Uuid::nil());
        let req = Bytes::from(vec![b'R'; threshold + 1]);
        let resp = Bytes::from_static(b"{\"ok\":true}");

        let (req_s, resp_s, ttl, loc) = resolve_cloud_log_bodies(&s3, 90, rid, org, &req, &resp)
            .await
            .expect("resolve");

        assert_eq!(loc, "s3");
        assert!(
            req_s.starts_with("http://") || req_s.starts_with("https://"),
            "expected presigned URL, got len {}",
            req_s.len()
        );
        assert!(
            resp_s.starts_with("http://") || resp_s.starts_with("https://"),
            "small response must still be S3 when request is large: {resp_s}"
        );
        assert_eq!(ttl, 90);

        info_log_request_summary(
            "request_body >= 1 MiB, response small",
            &req_s,
            &resp_s,
            ttl,
            &loc,
        );
    }

    #[tokio::test]
    async fn cloud_response_over_1_mib_yields_s3_and_presigned_response_field() {
        let threshold = LARGE_BODY_THRESHOLD_BYTES;
        let (_mock, s3) = start_s3_mock(2.into()).await;
        let rid = Uuid::nil();
        let org = OrgId::new(Uuid::nil());
        let req = Bytes::from_static(br#"{"x":1}"#);
        let resp = Bytes::from(vec![b'S'; threshold + 1]);

        let (req_s, resp_s, ttl, loc) = resolve_cloud_log_bodies(&s3, 90, rid, org, &req, &resp)
            .await
            .expect("resolve");

        assert_eq!(loc, "s3");
        assert!(
            req_s.starts_with("http://") || req_s.starts_with("https://"),
            "small request must still be S3 when response is large: {req_s}"
        );
        assert!(
            resp_s.starts_with("http://") || resp_s.starts_with("https://"),
            "expected presigned URL"
        );

        info_log_request_summary(
            "request small, response_body >= 1 MiB",
            &req_s,
            &resp_s,
            ttl,
            &loc,
        );
    }

    #[tokio::test]
    async fn cloud_both_under_1_mib_clickhouse_inline_no_put() {
        let (_mock, s3) = start_s3_mock(0.into()).await;
        let rid = Uuid::nil();
        let org = OrgId::new(Uuid::nil());
        let req = Bytes::from_static(b"hello");
        let resp = Bytes::from_static(b"world");

        let (req_s, resp_s, ttl, loc) = resolve_cloud_log_bodies(&s3, 90, rid, org, &req, &resp)
            .await
            .expect("resolve");

        assert_eq!(loc, "clickhouse");
        assert_eq!(req_s, "hello");
        assert_eq!(resp_s, "world");
        assert_eq!(ttl, 90);

        info_log_request_summary("both bodies < 1 MiB", &req_s, &resp_s, ttl, &loc);
    }

    #[tokio::test]
    async fn cloud_both_over_1_mib_two_puts_presigned_both_sides() {
        let threshold = LARGE_BODY_THRESHOLD_BYTES;
        let (_mock, s3) = start_s3_mock(2.into()).await;
        let rid = Uuid::nil();
        let org = OrgId::new(Uuid::nil());
        let req = Bytes::from(vec![b'A'; threshold]);
        let resp = Bytes::from(vec![b'B'; threshold]);

        let (req_s, resp_s, ttl, loc) = resolve_cloud_log_bodies(&s3, 90, rid, org, &req, &resp)
            .await
            .expect("resolve");

        assert_eq!(loc, "s3");
        assert!(req_s.starts_with("http://") || req_s.starts_with("https://"));
        assert!(resp_s.starts_with("http://") || resp_s.starts_with("https://"));

        info_log_request_summary("both bodies >= 1 MiB", &req_s, &resp_s, ttl, &loc);
    }
}
