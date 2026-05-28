#![allow(
    clippy::doc_markdown,
    clippy::uninlined_format_args,
    clippy::too_many_arguments
)]
//! Smoke test Cloud log-body paths with **real** S3-compatible storage (local
//! MinIO or Alibaba OSS). If both sides are under 1 MiB they are inline; if
//! **either side** reaches 1 MiB then **both** raw bodies are PUT and returned
//! as presigned GET URLs.
//!
//! **Exits immediately by default** (no env var means no network access and no
//! resource use).
//!
//! # Local MinIO (API often `:9000`, console often `:9001`)
//!
//! With `STORAGE_BACKEND=minio`, this example attempts to **auto-create the
//! bucket** (S3 CreateBucket; account permissions required). Default bucket
//! name is `request-response-storage`. You can still verify objects in the web
//! **Object Browser**: `organizations/<org>/ttl-<n>/requests/<request_id>/
//! request_body|response_body`.
//!
//! Env keys are read in flat-first order: `S3_*` is preferred; the legacy
//! nested form `AI_GATEWAY__S3__*` is still accepted as a fallback.
//!
//! ```text
//! CLOUD_LOG_STORAGE_TEST=1 STORAGE_BACKEND=minio \
//!   S3_ACCESS_KEY=minioadmin S3_SECRET_KEY=minioadmin \
//!   cargo run -p ai-gateway --example cloud_log_bodies_real_storage
//! ```
//!
//! Example with API on **9999**:
//!
//! ```text
//! CLOUD_LOG_STORAGE_TEST=1 STORAGE_BACKEND=minio \
//!   S3_ENDPOINT=http://127.0.0.1:9999 \
//!   S3_ACCESS_KEY=minioadmin S3_SECRET_KEY=minioadmin \
//!   cargo run -p ai-gateway --example cloud_log_bodies_real_storage
//! ```
//!
//! Optional: `S3_ENDPOINT` (default `http://127.0.0.1:9000`),
//! `S3_BUCKET_NAME`, `S3_REGION` (each also accepts the
//! `AI_GATEWAY__S3__*` form as fallback).
//!
//! # Alibaba OSS (S3-compatible endpoint)
//!
//! Usually use **Path** style against `https://oss-cn-<region>.aliyuncs.com`; if it fails, try
//! `OSS_URL_STYLE=virtual-host` and verify with
//! [OSS S3 compatibility docs](https://help.aliyun.com/zh/oss/developer-reference/use-amazon-s3-sdks-to-access-oss).
//!
//! ```text
//! CLOUD_LOG_STORAGE_TEST=1 STORAGE_BACKEND=oss \
//!   OSS_ENDPOINT=https://oss-cn-hangzhou.aliyuncs.com \
//!   OSS_BUCKET=<bucket> OSS_REGION=oss-cn-hangzhou \
//!   OSS_ACCESS_KEY_ID=... OSS_ACCESS_KEY_SECRET=... \
//!   cargo run -p ai-gateway --example cloud_log_bodies_real_storage
//! ```
//!
//! # Run MinIO + OSS sequentially
//!
//! ```text
//! CLOUD_LOG_STORAGE_TEST=1 STORAGE_BACKEND=both \
//!   S3_ACCESS_KEY=... S3_SECRET_KEY=... \
//!   OSS_ENDPOINT=... OSS_BUCKET=... OSS_REGION=... OSS_ACCESS_KEY_ID=... OSS_ACCESS_KEY_SECRET=... \
//!   cargo run -p ai-gateway --example cloud_log_bodies_real_storage
//! ```

use std::{
    io::{self, Write},
    time::SystemTime,
};

use ai_gateway::{
    config::s3::{Config, UrlStyle},
    logger::{body_storage::LARGE_BODY_THRESHOLD_BYTES, cloud_bodies::resolve_cloud_log_bodies},
    store::s3::BaseS3Client,
    types::{org::OrgId, secret::Secret},
};
use aws_credential_types::Credentials;
use aws_sigv4::{
    http_request::{SignableBody, SignableRequest, SigningSettings},
    sign::v4,
};
use bytes::Bytes;
use http::Method;
use reqwest::Client;
use url::Url;
use uuid::Uuid;

fn main() -> Result<(), String> {
    if std::env::var("CLOUD_LOG_STORAGE_TEST").ok().as_deref() != Some("1") {
        eprintln!(
            "Skipped: set CLOUD_LOG_STORAGE_TEST=1 to access S3-compatible \
             storage (including MinIO) / OSS. See module docs at the top of \
             examples/cloud_log_bodies_real_storage.rs."
        );
        return Ok(());
    }

    let backend = std::env::var("STORAGE_BACKEND").unwrap_or_else(|_| "minio".to_string());
    let backends: Vec<&str> = match backend.as_str() {
        "both" => vec!["minio", "oss"],
        "minio" | "oss" => vec![backend.as_str()],
        _ => {
            return Err(format!(
                "STORAGE_BACKEND must be minio, oss, or both; got {backend:?}"
            ));
        }
    };

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?
        .block_on(async move { run_backends(&backends).await })?;

    Ok(())
}

async fn run_backends(backends: &[&str]) -> Result<(), String> {
    let http_client = Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())?;

    for b in backends {
        eprintln!("\n======== backend: {b} ========");
        let config = match *b {
            "minio" => s3_compatible_config_from_env()?,
            "oss" => oss_config()?,
            _ => unreachable!(),
        };
        eprintln!(
            "Connecting: endpoint={} bucket={} url_style={:?}",
            config.endpoint, config.bucket_name, config.url_style
        );

        if *b == "minio" {
            eprintln!("Ensuring bucket exists (S3 CreateBucket / PUT)…");
            match ensure_bucket_exists(&config, &http_client).await {
                Ok(()) => {
                    eprintln!("Bucket \"{}\" is ready.", config.bucket_name);
                }
                Err(e) => {
                    eprintln!("Auto-create bucket failed: {e}");
                    eprintln!(
                        "Open the MinIO console (see hints below) and create \
                         a bucket with the same name, then retry."
                    );
                }
            }
        }

        let client =
            BaseS3Client::new(config.clone()).map_err(|e| format!("BaseS3Client::new: {e}"))?;
        run_scenarios(b, &config, &client).await?;
    }
    eprintln!("\nAll scenarios finished.");
    Ok(())
}

/// S3 CreateBucket: PUT `https://endpoint/bucket/` (path-style).
async fn ensure_bucket_exists(cfg: &Config, http_client: &Client) -> Result<(), String> {
    let bucket_url = bucket_root_url(cfg)?;
    let identity = Credentials::new(
        cfg.access_key.expose(),
        cfg.secret_key.expose(),
        None,
        None,
        "cloud-log-smoke",
    )
    .into();

    let signing_params = v4::SigningParams::builder()
        .identity(&identity)
        .region(cfg.region.as_str())
        .name("s3")
        .time(SystemTime::now())
        .settings(SigningSettings::default())
        .build()
        .map_err(|e| e.to_string())?
        .into();

    let host = bucket_url.host_str().ok_or("bucket URL missing host")?;
    let authority = match bucket_url.port() {
        Some(p) => format!("{host}:{p}"),
        None => host.to_string(),
    };

    let signable = SignableRequest::new(
        "PUT",
        bucket_url.as_str(),
        [("host", authority.as_str())].into_iter(),
        SignableBody::Bytes(&[]),
    )
    .map_err(|e| e.to_string())?;

    let (signing_output, _) = aws_sigv4::http_request::sign(signable, &signing_params)
        .map_err(|e| e.to_string())?
        .into_parts();

    let mut req = ::http::Request::builder()
        .method(Method::PUT)
        .uri(bucket_url.as_str())
        .body(Bytes::new())
        .map_err(|e| e.to_string())?;
    signing_output.apply_to_request_http1x(&mut req);

    let mut rb = http_client.put(bucket_url.clone());
    for (k, v) in req.headers() {
        if let Ok(s) = v.to_str() {
            rb = rb.header(k.as_str(), s);
        }
    }

    let resp = rb.send().await.map_err(|e| e.to_string())?;
    let status = resp.status();
    if status.is_success() || status.as_u16() == 409 {
        Ok(())
    } else {
        let body = resp.text().await.unwrap_or_default();
        Err(format!("HTTP {status} {body}"))
    }
}

fn bucket_root_url(cfg: &Config) -> Result<Url, String> {
    let base = cfg.endpoint.as_str().trim_end_matches('/');
    let name = cfg.bucket_name.trim_matches('/');
    Url::parse(&format!("{base}/{name}/")).map_err(|e| format!("bucket URL: {e}"))
}

fn s3_compatible_console_hint(api: &Url) -> String {
    let scheme = api.scheme();
    let host = api.host_str().unwrap_or("127.0.0.1");
    let api_port = api.port_or_known_default().unwrap_or(9000);
    // Common deployment: API 9000, console 9001
    let ui_port = if api_port == 9000 { 9001 } else { api_port };
    format!("{scheme}://{host}:{ui_port}")
}

fn object_key_request(org: OrgId, ttl: u16, rid: Uuid) -> String {
    format!(
        "organizations/{org}/ttl-{ttl}/requests/{rid}/request_body",
        org = org,
        ttl = ttl,
        rid = rid
    )
}

fn object_key_response(org: OrgId, ttl: u16, rid: Uuid) -> String {
    format!(
        "organizations/{org}/ttl-{ttl}/requests/{rid}/response_body",
        org = org,
        ttl = ttl,
        rid = rid
    )
}

#[inline]
fn field_is_presigned_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

/// If either side is ≥ 1 MiB: `'s3'`, and both request/response fields are
/// presigned URLs.
fn assert_either_side_at_least_1_mib(
    scenario: &str,
    out: &(String, String, u16, String),
) -> Result<(), String> {
    let (req_s, resp_s, _ttl, loc) = out;
    if loc != "s3" {
        return Err(format!(
            "{scenario}: expected storage_location s3, got {loc:?}"
        ));
    }
    if !field_is_presigned_url(req_s) {
        return Err(format!(
            "{scenario}: expected request field to be a presigned URL prefix, \
             got prefix {:?}",
            req_s.chars().take(24).collect::<String>()
        ));
    }
    if !field_is_presigned_url(resp_s) {
        return Err(format!(
            "{scenario}: expected response field to be a presigned URL \
             prefix, got prefix {:?}",
            resp_s.chars().take(24).collect::<String>()
        ));
    }
    Ok(())
}

/// If both sides are < 1 MiB: `'clickhouse'`, and both sides stay inline (not
/// URLs).
fn assert_both_under_1_mib(
    scenario: &str,
    out: &(String, String, u16, String),
) -> Result<(), String> {
    let (req_s, resp_s, _ttl, loc) = out;
    if loc != "clickhouse" {
        return Err(format!(
            "{scenario}: expected storage_location clickhouse, got {loc:?}"
        ));
    }
    if field_is_presigned_url(req_s) || field_is_presigned_url(resp_s) {
        return Err(format!(
            "{scenario}: expected both bodies inline, not http(s) URLs \
             (request len {})",
            req_s.len()
        ));
    }
    Ok(())
}

async fn run_scenarios(backend: &str, cfg: &Config, client: &BaseS3Client) -> Result<(), String> {
    let rid = Uuid::now_v7();
    let org = OrgId::new(Uuid::nil());
    let ttl_days = 7_u16;

    let threshold = LARGE_BODY_THRESHOLD_BYTES;
    let large_buf = vec![b'm'; threshold + 1];

    if backend == "minio" {
        eprintln!(
            "\nView objects in the browser: open {} → sign in → Buckets → \
             \"{}\" → Browse; request_id = {rid}",
            s3_compatible_console_hint(&cfg.endpoint),
            cfg.bucket_name
        );
    }

    eprintln!("--- Scenario 1: large request body (≥ 1 MiB), small response ---");
    let req_b = Bytes::copy_from_slice(&large_buf);
    let resp_b = Bytes::from_static(br#"{"ok":true}"#);
    let out = resolve_cloud_log_bodies(client, ttl_days, rid, org, &req_b, &resp_b)
        .await
        .map_err(|e| format!("resolve: {e}"))?;
    print_log_fields_summary("request>=1MiB", &out);
    assert_either_side_at_least_1_mib("scenario1", &out)?;
    eprintln!(
        "Scenario 1 assertions passed: storage_location=s3, both sides \
         presigned URLs."
    );
    print_upload_hint(backend, cfg, org, ttl_days, rid, "scenario1", &out);
    drop(req_b);

    eprintln!("--- Scenario 2: small request body, large response (≥ 1 MiB) ---");
    let req_b = Bytes::from_static(br#"{"x":1}"#);
    let resp_b = Bytes::copy_from_slice(&large_buf);
    let out = resolve_cloud_log_bodies(client, ttl_days, rid, org, &req_b, &resp_b)
        .await
        .map_err(|e| format!("resolve: {e}"))?;
    print_log_fields_summary("response>=1MiB", &out);
    assert_either_side_at_least_1_mib("scenario2", &out)?;
    eprintln!(
        "Scenario 2 assertions passed: storage_location=s3, both sides \
         presigned URLs."
    );
    print_upload_hint(backend, cfg, org, ttl_days, rid, "scenario2", &out);

    eprintln!("--- Scenario 3: both sides < 1 MiB (no PUT) ---");
    let req_b = Bytes::from_static(b"hi");
    let resp_b = Bytes::from_static(b"bye");
    let out = resolve_cloud_log_bodies(client, ttl_days, rid, org, &req_b, &resp_b)
        .await
        .map_err(|e| format!("resolve: {e}"))?;
    print_log_fields_summary("both<1MiB", &out);
    assert_both_under_1_mib("scenario3", &out)?;
    eprintln!(
        "Scenario 3 assertions passed: storage_location=clickhouse, both \
         inline."
    );

    Ok(())
}

fn print_upload_hint(
    backend: &str,
    cfg: &Config,
    org: OrgId,
    ttl: u16,
    rid: Uuid,
    label: &str,
    out: &(String, String, u16, String),
) {
    let (req_s, resp_s, _ttl, loc) = out;
    if loc != "s3" {
        return;
    }
    let req_url = req_s.starts_with("http://") || req_s.starts_with("https://");
    let resp_url = resp_s.starts_with("http://") || resp_s.starts_with("https://");
    if backend != "minio" {
        return;
    }
    eprintln!(
        ">>> {label} wrote object keys under bucket \"{}\" (expand path in \
         console):",
        cfg.bucket_name
    );
    if req_url {
        eprintln!("    {}", object_key_request(org, ttl, rid));
    }
    if resp_url {
        eprintln!("    {}", object_key_response(org, ttl, rid));
    }
    let _ = io::stderr().flush();
}

fn print_log_fields_summary(scenario: &str, out: &(String, String, u16, String)) {
    let (req_s, resp_s, ttl, loc) = out;
    let summarize = |s: &str| -> serde_json::Value {
        if s.starts_with("http://") || s.starts_with("https://") {
            serde_json::json!({
                "kind": "presigned_get_url",
                "byte_len": s.len(),
                "prefix": s.chars().take(160).collect::<String>(),
            })
        } else {
            serde_json::json!({
                "kind": "inline_utf8",
                "byte_len": s.len(),
                "preview": s.chars().take(120).collect::<String>(),
            })
        }
    };
    let payload = serde_json::json!({
        "scenario": scenario,
        "log.request.storageLocation": loc,
        "log.request.bodyTtlDays": ttl,
        "log.request.requestBody": summarize(req_s),
        "log.request.responseBody": summarize(resp_s),
    });
    let pretty = serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".to_string());
    eprintln!("{pretty}");
    let _ = io::stderr().flush();
}

fn env_s3_optional(flat: &str, nested: &str) -> Option<String> {
    if let Some(v) = std::env::var(flat)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        return Some(v);
    }
    std::env::var(nested)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn env_s3_required(flat: &str, nested: &str) -> Result<String, String> {
    env_s3_optional(flat, nested).ok_or_else(|| format!("missing {flat} or {nested}"))
}

fn s3_compatible_config_from_env() -> Result<Config, String> {
    let endpoint = env_s3_optional("S3_ENDPOINT", "AI_GATEWAY__S3__ENDPOINT")
        .unwrap_or_else(|| "http://127.0.0.1:9000".to_string());
    let access = env_s3_required("S3_ACCESS_KEY", "AI_GATEWAY__S3__ACCESS_KEY")?;
    let secret = env_s3_required("S3_SECRET_KEY", "AI_GATEWAY__S3__SECRET_KEY")?;
    let bucket = env_s3_optional("S3_BUCKET_NAME", "AI_GATEWAY__S3__BUCKET_NAME")
        .unwrap_or_else(|| "request-response-storage".to_string());
    let region = env_s3_optional("S3_REGION", "AI_GATEWAY__S3__REGION")
        .unwrap_or_else(|| "us-east-1".to_string());
    Ok(Config {
        url_style: UrlStyle::Path,
        bucket_name: bucket,
        endpoint: Url::parse(&endpoint)
            .map_err(|e| format!("S3_ENDPOINT/AI_GATEWAY__S3__ENDPOINT: {e}"))?,
        region,
        access_key: Secret::from(access),
        secret_key: Secret::from(secret),
    })
}

fn oss_url_style() -> Result<UrlStyle, String> {
    match std::env::var("OSS_URL_STYLE")
        .unwrap_or_else(|_| "path".to_string())
        .as_str()
    {
        "path" => Ok(UrlStyle::Path),
        "virtual-host" | "virtualhost" => Ok(UrlStyle::VirtualHost),
        other => Err(format!("OSS_URL_STYLE unknown value: {other}")),
    }
}

fn oss_config() -> Result<Config, String> {
    let host = std::env::var("OSS_ENDPOINT").map_err(|_| "missing OSS_ENDPOINT".to_string())?;
    let access =
        std::env::var("OSS_ACCESS_KEY_ID").map_err(|_| "missing OSS_ACCESS_KEY_ID".to_string())?;
    let secret = std::env::var("OSS_ACCESS_KEY_SECRET")
        .map_err(|_| "missing OSS_ACCESS_KEY_SECRET".to_string())?;
    let bucket = std::env::var("OSS_BUCKET").map_err(|_| "missing OSS_BUCKET".to_string())?;
    let region = std::env::var("OSS_REGION").unwrap_or_else(|_| "oss-cn-hangzhou".to_string());
    Ok(Config {
        url_style: oss_url_style()?,
        bucket_name: bucket,
        endpoint: Url::parse(&host).map_err(|e| format!("OSS_ENDPOINT: {e}"))?,
        region,
        access_key: Secret::from(access),
        secret_key: Secret::from(secret),
    })
}
