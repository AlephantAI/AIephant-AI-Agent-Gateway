const MAX_PUT_ATTEMPTS: u32 = 4;

/// Outcome when classified put logic stops before success.
#[derive(Debug)]
pub enum PutClassifiedError {
    /// 429 / 5xx exhausted in-loop retries (still "transient" for lazy
    /// wrapper).
    TransientExhausted(String),
    /// Non-retryable HTTP status.
    Terminal(String),
}

/// PUT with exponential backoff on 429 / 5xx; classifies terminal vs transient
/// exhaustion.
pub async fn put_with_backoff_classified(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    body: &str,
    ttl: u64,
) -> Result<(), PutClassifiedError> {
    let ttl = ttl.max(1);
    let mut attempt = 0u32;
    loop {
        let res = client
            .put(url)
            .query(&[("expiration_ttl", ttl.to_string())])
            .header("Authorization", format!("Bearer {token}"))
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| {
                PutClassifiedError::TransientExhausted(e.to_string())
            })?;
        let status = res.status();
        if status.is_success() {
            return Ok(());
        }
        if status.as_u16() == 429 || status.is_server_error() {
            attempt += 1;
            if attempt >= MAX_PUT_ATTEMPTS {
                return Err(PutClassifiedError::TransientExhausted(format!(
                    "put failed after retries: {status}"
                )));
            }
            let delay_ms = (1u64 << attempt) * 500;
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms))
                .await;
            continue;
        }
        return Err(PutClassifiedError::Terminal(format!(
            "put failed: {status}"
        )));
    }
}

/// PUT with exponential backoff on 429 / 5xx.
pub async fn put_with_backoff(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    body: &str,
    ttl: u64,
) -> Result<(), crate::error::LlmKvCacheError> {
    put_with_backoff_classified(client, url, token, body, ttl)
        .await
        .map_err(|e| match e {
            PutClassifiedError::TransientExhausted(s)
            | PutClassifiedError::Terminal(s) => {
                crate::error::LlmKvCacheError::Http(s)
            }
        })
}
