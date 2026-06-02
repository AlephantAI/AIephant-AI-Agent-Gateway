#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingIdentity {
    pub provider: String,
    pub model: String,
    pub provider_defaulted: bool,
}

impl EmbeddingIdentity {
    #[must_use]
    pub fn model_for_openai_compatible_api(&self) -> &str {
        &self.model
    }

    #[must_use]
    pub fn params_hash_identity(&self, base_url: &str, dimension: usize) -> String {
        format!(
            "provider={}\0model={}\0base_url={}\0dimension={}",
            self.provider,
            self.model,
            base_url.trim_end_matches('/'),
            dimension
        )
    }
}

pub fn parse_embedding_identity(raw: &str) -> Result<EmbeddingIdentity, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("embedding model is empty".to_string());
    }

    let (provider, model, provider_defaulted) =
        if let Some((provider, model)) = trimmed.split_once('/') {
            (provider.trim(), model.trim(), false)
        } else {
            ("openai", trimmed, true)
        };

    if provider.is_empty() {
        return Err("embedding provider is empty".to_string());
    }
    if model.is_empty() {
        return Err("embedding model is empty".to_string());
    }

    Ok(EmbeddingIdentity {
        provider: provider.to_string(),
        model: model.to_string(),
        provider_defaulted,
    })
}

pub fn collection_name_for_embedding(
    identity: &EmbeddingIdentity,
    dimension: usize,
) -> Result<String, String> {
    if dimension == 0 {
        return Err("embedding dimension must be greater than zero".to_string());
    }
    let provider = slug(&identity.provider)?;
    let model = slug(&identity.model)?;
    Ok(format!("semantic_cache__{provider}__{model}__{dimension}"))
}

fn slug(input: &str) -> Result<String, String> {
    let mut out = String::with_capacity(input.len());
    let mut last_was_underscore = false;

    for ch in input.chars().flat_map(char::to_lowercase) {
        let safe = ch.is_ascii_alphanumeric() || ch == '-' || ch == '_';
        if ch == '_' {
            if !last_was_underscore {
                out.push(ch);
            }
            last_was_underscore = true;
        } else if safe {
            out.push(ch);
            last_was_underscore = false;
        } else if !last_was_underscore {
            out.push('_');
            last_was_underscore = true;
        }
    }

    let trimmed = out.trim_matches('_').to_string();
    if trimmed.is_empty() {
        return Err("embedding identity slug is empty".to_string());
    }
    Ok(trimmed)
}

#[cfg(test)]
mod tests {
    use super::{EmbeddingIdentity, collection_name_for_embedding, parse_embedding_identity};

    #[test]
    fn parse_embedding_identity_keeps_explicit_provider() {
        let parsed = parse_embedding_identity("voyage/voyage-3-lite").expect("identity");

        assert_eq!(
            parsed,
            EmbeddingIdentity {
                provider: "voyage".to_string(),
                model: "voyage-3-lite".to_string(),
                provider_defaulted: false,
            }
        );
    }

    #[test]
    fn parse_embedding_identity_defaults_provider_to_openai() {
        let parsed = parse_embedding_identity("text-embedding-3-small").expect("identity");

        assert_eq!(parsed.provider, "openai");
        assert_eq!(parsed.model, "text-embedding-3-small");
        assert!(parsed.provider_defaulted);
    }

    #[test]
    fn parse_embedding_identity_rejects_empty_model() {
        let err = parse_embedding_identity("openai/").unwrap_err();

        assert_eq!(err, "embedding model is empty");
    }

    #[test]
    fn collection_name_includes_provider_model_and_dimension() {
        let identity = parse_embedding_identity("openai/text-embedding-3-large").unwrap();

        assert_eq!(
            collection_name_for_embedding(&identity, 3072).unwrap(),
            "semantic_cache__openai__text-embedding-3-large__3072"
        );
    }

    #[test]
    fn collection_name_sanitizes_provider_and_model() {
        let identity = EmbeddingIdentity {
            provider: "Azure East/Prod".to_string(),
            model: "Text Embedding@Large".to_string(),
            provider_defaulted: false,
        };

        assert_eq!(
            collection_name_for_embedding(&identity, 1536).unwrap(),
            "semantic_cache__azure_east_prod__text_embedding_large__1536"
        );
    }

    #[test]
    fn collection_name_collapses_existing_underscores() {
        let identity = EmbeddingIdentity {
            provider: "Foo__Bar".to_string(),
            model: "Model__Name".to_string(),
            provider_defaulted: false,
        };

        assert_eq!(
            collection_name_for_embedding(&identity, 1536).unwrap(),
            "semantic_cache__foo_bar__model_name__1536"
        );
    }

    #[test]
    fn collection_name_rejects_zero_dimension() {
        let identity = parse_embedding_identity("openai/text-embedding-3-small").unwrap();

        assert_eq!(
            collection_name_for_embedding(&identity, 0).unwrap_err(),
            "embedding dimension must be greater than zero"
        );
    }
}
