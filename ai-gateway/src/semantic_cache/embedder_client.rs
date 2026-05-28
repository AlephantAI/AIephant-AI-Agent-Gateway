use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Clone)]
pub struct OpenAiEmbedderClient {
    pub client: reqwest::Client,
}

impl OpenAiEmbedderClient {
    #[must_use]
    pub fn new(client: reqwest::Client) -> Self {
        Self { client }
    }

    pub async fn embed(
        &self,
        base_url: &str,
        api_key: &str,
        model: &str,
        input: &str,
    ) -> Result<Vec<f32>, String> {
        let url = build_embeddings_url(base_url);
        let request_body = json!({
            "model": model,
            "input": input
        });
        tracing::info!(
            target: "semantic_cache::embedder_client",
            url = %url,
            model = %model,
            request_body = %request_body,
            "requesting embeddings model"
        );
        let resp = self
            .client
            .post(&url)
            .bearer_auth(api_key)
            .json(&request_body)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = resp.status();
        let response_body = resp.text().await.map_err(|e| e.to_string())?;
        tracing::info!(
            target: "semantic_cache::embedder_client",
            url = %url,
            status = %status,
            "embeddings response received"
        );
        if !status.is_success() {
            return Err(format!("openai embeddings failed: {status}"));
        }
        let parsed: EmbeddingResponse =
            serde_json::from_str(&response_body).map_err(|e| e.to_string())?;
        let Some(first) = parsed.data.into_iter().next() else {
            return Err("openai embeddings response has no vectors".to_string());
        };
        Ok(first.embedding)
    }
}

/// If `base_url` already ends with a version segment (`/v1`, `/v2`, …),
/// append only `/embeddings`; otherwise prepend `/v1`.
fn build_embeddings_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    let last_segment = trimmed.rsplit('/').next().unwrap_or("");
    let has_version = last_segment.len() >= 2
        && last_segment.starts_with('v')
        && last_segment[1..].chars().all(|c| c.is_ascii_digit());
    if has_version {
        format!("{trimmed}/embeddings")
    } else {
        format!("{trimmed}/v1/embeddings")
    }
}

#[derive(Debug, Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingItem>,
}

#[derive(Debug, Deserialize)]
struct EmbeddingItem {
    embedding: Vec<f32>,
}

#[cfg(test)]
mod tests {
    use super::{OpenAiEmbedderClient, build_embeddings_url};

    #[test]
    fn url_without_version_segment() {
        assert_eq!(
            build_embeddings_url("https://api.openai.com"),
            "https://api.openai.com/v1/embeddings"
        );
        assert_eq!(
            build_embeddings_url("https://api.openai.com/"),
            "https://api.openai.com/v1/embeddings"
        );
    }

    #[test]
    fn url_with_version_segment() {
        assert_eq!(
            build_embeddings_url("https://api.openai.com/v1"),
            "https://api.openai.com/v1/embeddings"
        );
        assert_eq!(
            build_embeddings_url("https://api.openai.com/v1/"),
            "https://api.openai.com/v1/embeddings"
        );
        assert_eq!(
            build_embeddings_url("https://api.openai.com/v2"),
            "https://api.openai.com/v2/embeddings"
        );
    }

    #[test]
    fn url_with_custom_path_no_version() {
        assert_eq!(
            build_embeddings_url("https://my-proxy.example.com/api"),
            "https://my-proxy.example.com/api/v1/embeddings"
        );
    }

    #[tokio::test]
    async fn embed_returns_error_on_bad_base_url() {
        let c = OpenAiEmbedderClient::new(reqwest::Client::new());
        let err = c
            .embed(
                "http://127.0.0.1:1",
                "sk-test",
                "text-embedding-3-large",
                "hello",
            )
            .await
            .unwrap_err();
        assert!(!err.is_empty());
    }
}
