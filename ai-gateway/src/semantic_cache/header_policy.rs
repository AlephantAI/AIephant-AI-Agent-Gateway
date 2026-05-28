use http::HeaderMap;

const ALEPHANT_CACHE_SEMANTIC_THRESHOLD: &str = "Alephant-Cache-Semantic-Threshold";
const ALEPHANT_CACHE_TTL: &str = "Alephant-Cache-Ttl";

#[derive(Debug, Clone, PartialEq)]
pub struct SemanticPolicy {
    pub threshold: f32,
    pub ttl_seconds: u64,
}

impl SemanticPolicy {
    pub fn from_headers(
        headers: &HeaderMap,
        default_threshold: f32,
        default_ttl_seconds: u64,
    ) -> Result<Self, String> {
        let threshold = parse_threshold(headers, default_threshold)?;
        let ttl_seconds = parse_ttl(headers, default_ttl_seconds)?;
        Ok(Self {
            threshold,
            ttl_seconds,
        })
    }
}

fn parse_threshold(headers: &HeaderMap, default_threshold: f32) -> Result<f32, String> {
    let Some(raw) = headers.get(ALEPHANT_CACHE_SEMANTIC_THRESHOLD) else {
        return Ok(default_threshold);
    };
    let s = raw
        .to_str()
        .map_err(|_| "invalid Alephant-Cache-Semantic-Threshold".to_string())?;
    let v = s
        .parse::<f32>()
        .map_err(|_| "invalid Alephant-Cache-Semantic-Threshold".to_string())?;
    if !(0.0..=1.0).contains(&v) {
        return Err("Alephant-Cache-Semantic-Threshold out of range".to_string());
    }
    Ok(v)
}

fn parse_ttl(headers: &HeaderMap, default_ttl_seconds: u64) -> Result<u64, String> {
    let Some(raw) = headers.get(ALEPHANT_CACHE_TTL) else {
        return Ok(default_ttl_seconds);
    };
    let s = raw
        .to_str()
        .map_err(|_| "invalid Alephant-Cache-Ttl".to_string())?;
    s.parse::<u64>()
        .map_err(|_| "invalid Alephant-Cache-Ttl".to_string())
}

#[cfg(test)]
mod tests {
    use http::HeaderMap;

    use super::SemanticPolicy;

    #[test]
    fn parse_policy_uses_defaults_when_headers_missing() {
        let headers = HeaderMap::new();
        let p = SemanticPolicy::from_headers(&headers, 0.9, 3600).unwrap();
        assert_eq!(p.threshold, 0.9);
        assert_eq!(p.ttl_seconds, 3600);
    }

    #[test]
    fn parse_policy_allows_alephant_header_overrides() {
        let mut headers = HeaderMap::new();
        headers.insert("Alephant-Cache-Semantic-Threshold", "0.82".parse().unwrap());
        headers.insert("Alephant-Cache-Ttl", "120".parse().unwrap());
        let p = SemanticPolicy::from_headers(&headers, 0.9, 3600).unwrap();
        assert_eq!(p.threshold, 0.82);
        assert_eq!(p.ttl_seconds, 120);
    }
}
