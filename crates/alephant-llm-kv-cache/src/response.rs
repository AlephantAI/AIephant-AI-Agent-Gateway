use std::collections::HashMap;

use http::{HeaderMap, HeaderName, HeaderValue};

/// Copy provider response headers from a cache entry into `dst`.
pub fn merge_cached_headers(dst: &mut HeaderMap, src: &HashMap<String, String>) {
    for (k, v) in src {
        let Ok(name) = HeaderName::try_from(k.as_str()) else {
            continue;
        };
        let Ok(val) = HeaderValue::try_from(v.as_str()) else {
            continue;
        };
        dst.insert(name, val);
    }
}

/// Append Alephant cache HIT markers to a cached response.
pub fn apply_alephant_cache_hit_headers(dst: &mut HeaderMap, bucket_idx: usize, latency_ms: u64) {
    let _ = dst.insert(
        HeaderName::from_static("alephant-cache"),
        HeaderValue::from_static("HIT"),
    );
    if let Ok(v) = HeaderValue::try_from(bucket_idx.to_string()) {
        let _ = dst.insert(HeaderName::from_static("alephant-cache-bucket-idx"), v);
    }
    if let Ok(v) = HeaderValue::try_from(latency_ms.to_string()) {
        let _ = dst.insert(HeaderName::from_static("alephant-cache-latency"), v);
    }
}
