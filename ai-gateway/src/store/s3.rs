use std::time::Duration;

use bytes::Bytes;
use reqwest::Client;
use rusty_s3::{
    Bucket, Credentials, S3Action,
    actions::{GetObject, PutObject},
};
use url::Url;

use crate::{
    app_state::AppState,
    config::s3::Config,
    error::{init::InitError, logger::LoggerError, prompts::PromptError},
    types::extensions::AuthContext,
};

const DEFAULT_S3_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug)]
pub struct BaseS3Client {
    pub bucket: Bucket,
    pub client: Client,
    pub credentials: Credentials,
}

impl BaseS3Client {
    pub fn new(config: Config) -> Result<Self, InitError> {
        let bucket = Bucket::new(
            config.endpoint,
            config.url_style.into(),
            config.bucket_name,
            config.region,
        )?;
        let client = Client::builder()
            .connect_timeout(DEFAULT_S3_TIMEOUT)
            .tcp_nodelay(true)
            .build()
            .map_err(InitError::CreateReqwestClient)?;
        let credentials = Credentials::new(config.access_key.expose(), config.secret_key.expose());
        Ok(Self {
            bucket,
            client,
            credentials,
        })
    }

    #[must_use]
    pub fn put_object<'obj, 'client>(&'client self, object: &'obj str) -> PutObject<'obj>
    where
        'client: 'obj,
    {
        PutObject::new(&self.bucket, Some(&self.credentials), object)
    }

    #[must_use]
    pub fn get_object<'obj, 'client>(&'client self, object: &'obj str) -> GetObject<'obj>
    where
        'client: 'obj,
    {
        GetObject::new(&self.bucket, Some(&self.credentials), object)
    }

    /// Presigned PUT URL for uploading a raw object (Cloud log bodies).
    #[must_use]
    pub fn sign_put_url_for_object(&self, object_key: &str) -> Url {
        self.put_object(object_key).sign(PUT_OBJECT_SIGN_DURATION)
    }

    /// PUT raw bytes to `put_url`, then return a presigned GET URL for the same
    /// object key. Used for Cloud log bodies >= 1 MiB (see design: inline vs
    /// OSS split). `put_url` must be the presigned PUT for `get_object_key`.
    pub async fn put_log_body_object_and_presign_get(
        &self,
        put_url: Url,
        get_object_key: &str,
        bytes: Bytes,
        content_type: &str,
    ) -> Result<Url, LoggerError> {
        let _resp = self
            .client
            .put(put_url)
            .header("Content-Type", content_type)
            .body(bytes)
            .send()
            .await
            .map_err(|e| {
                tracing::debug!(error = %e, "failed to put log body object to S3");
                LoggerError::FailedToSendRequest(e)
            })?
            .error_for_status()
            .map_err(|e| {
                tracing::error!(error = %e, "log body object PUT returned error status");
                LoggerError::ResponseError(e)
            })?;

        let action = self.get_object(get_object_key);
        Ok(action.sign(LOG_BODY_GET_SIGN_DURATION))
    }
}

const PUT_OBJECT_SIGN_DURATION: Duration = Duration::from_secs(120);
const GET_OBJECT_SIGN_DURATION: Duration = Duration::from_secs(120);

/// Presigned GET lifetime for Cloud log body download URLs embedded in JSON.
// TODO: make configurable (design §4.3).
const LOG_BODY_GET_SIGN_DURATION: Duration = Duration::from_secs(3600);

pub struct S3Client<'a>(&'a BaseS3Client);

impl<'a> S3Client<'a> {
    #[must_use]
    pub fn cloud(client: &'a BaseS3Client) -> Self {
        Self(client)
    }

    #[tracing::instrument(skip_all)]
    pub async fn pull_prompt_body(
        &self,
        app_state: &AppState,
        auth_ctx: &AuthContext,
        prompt_id: &str,
        version_id: &str,
    ) -> Result<serde_json::Value, PromptError> {
        let object_path = format!(
            "organizations/{}/prompts/{}/versions/{}/prompt_body",
            auth_ctx.org_id.as_ref(),
            prompt_id,
            version_id,
        );

        let action = self.0.get_object(&object_path);
        let signed_url = action.sign(GET_OBJECT_SIGN_DURATION);

        let response = app_state
            .0
            .s3
            .client
            .get(signed_url)
            .send()
            .await
            .map_err(|e| {
                tracing::debug!(error = %e, "failed to send request to S3 for prompt body");
                PromptError::FailedToSendRequest(e)
            })?
            .error_for_status()
            .map_err(|e| {
                tracing::error!(error = %e, "failed to get prompt body from S3");
                PromptError::FailedToGetPromptBody(e)
            })?;

        let response_bytes = response.bytes().await.map_err(|e| {
            tracing::error!(error = %e, "failed to read prompt body bytes");
            PromptError::FailedToGetPromptBody(e)
        })?;

        serde_json::from_slice(&response_bytes).map_err(|e| {
            tracing::error!(error = %e, "failed to deserialize prompt body JSON");
            PromptError::UnexpectedResponse(e.to_string())
        })
    }
}
