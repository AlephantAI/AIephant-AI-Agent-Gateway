use http::HeaderMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheSettings {
    pub should_read: bool,
    pub should_write: bool,
    pub cache_control_value: String,
    pub bucket_size: u8,
    pub cache_seed: Option<String>,
}

impl CacheSettings {
    pub fn parse(headers: &HeaderMap) -> Result<Self, String> {
        fn lower_get(headers: &HeaderMap, name: &str) -> Option<String> {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(std::string::ToString::to_string)
        }
        let enabled = lower_get(headers, "Alephant-Cache-Enabled")
            .is_some_and(|s| s.eq_ignore_ascii_case("true"));
        let save = lower_get(headers, "Alephant-Cache-Save")
            .is_some_and(|s| s.eq_ignore_ascii_case("true"));
        let read = lower_get(headers, "Alephant-Cache-Read")
            .is_some_and(|s| s.eq_ignore_ascii_case("true"));
        let bucket: u8 = lower_get(headers, "Alephant-Cache-Bucket-Max-Size")
            .and_then(|s| s.parse().ok())
            .unwrap_or(1);
        if bucket == 0 || bucket > 20 {
            return Err(format!(
                "Cache bucket size must be 1..=20, got {bucket}"
            ));
        }
        let seed = lower_get(headers, "Alephant-Cache-Seed");
        let cc_in =
            lower_get(headers, "Alephant-Cache-Control").unwrap_or_default();
        let cache_control_value = build_cache_control(&cc_in);
        Ok(Self {
            should_read: enabled || read,
            should_write: enabled || save,
            cache_control_value,
            bucket_size: bucket,
            cache_seed: seed,
        })
    }

    #[must_use]
    pub fn expiration_ttl_secs(&self) -> u64 {
        parse_max_age_seconds(&self.cache_control_value).unwrap_or(60)
    }
}

const MAX_CACHE_AGE: u64 = 60 * 60 * 24 * 365;
const DEFAULT_CACHE_AGE: u64 = 60 * 60 * 24 * 7;

fn build_cache_control(cache_control: &str) -> String {
    let s_max = regex_once(r"s-maxage=(\d+)", cache_control);
    let max = regex_once(r"max-age=(\d+)", cache_control);
    let sec = s_max.or(max).unwrap_or(DEFAULT_CACHE_AGE);
    let sec = sec.min(MAX_CACHE_AGE);
    format!("public, max-age={sec}")
}

fn regex_once(pattern: &str, hay: &str) -> Option<u64> {
    let re = regex::Regex::new(pattern).ok()?;
    re.captures(hay)?.get(1)?.as_str().parse::<u64>().ok()
}

fn parse_max_age_seconds(cache_control: &str) -> Option<u64> {
    regex_once(r"max-age=(\d+)", cache_control)
}

#[cfg(test)]
mod tests {
    use http::header::HeaderName;

    use super::*;

    #[test]
    fn bucket_over_20_errors() {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("alephant-cache-enabled"),
            http::HeaderValue::from_static("true"),
        );
        headers.insert(
            HeaderName::from_static("alephant-cache-bucket-max-size"),
            http::HeaderValue::from_static("21"),
        );
        let err = CacheSettings::parse(&headers).unwrap_err();
        assert!(err.contains("20"));
    }

    #[test]
    fn alephant_cache_control_sets_ttl() {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("alephant-cache-enabled"),
            http::HeaderValue::from_static("true"),
        );
        headers.insert(
            HeaderName::from_static("alephant-cache-control"),
            http::HeaderValue::from_static("max-age=120"),
        );
        let s = CacheSettings::parse(&headers).unwrap();
        assert_eq!(s.expiration_ttl_secs(), 120);
    }
}
