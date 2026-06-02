use bytes::Bytes;

/// 1 MiB (matches design §2).
pub const LARGE_BODY_THRESHOLD_BYTES: usize = 1024 * 1024;

#[must_use]
pub fn clamp_body_ttl_days(days: u16) -> u16 {
    let clamped = days.clamp(1, 730);
    if !(1..=730).contains(&days) {
        tracing::warn!(
            raw_body_ttl_days = days,
            clamped_body_ttl_days = clamped,
            "body_ttl_days out of range; clamped to 1..=730"
        );
    }
    clamped
}

/// If either side ≥ threshold → `"s3"` (Cloud: **both** bodies go to object
/// storage), else `"clickhouse"` (inline on both sides).
#[must_use]
pub fn storage_location_for_sizes(
    req_len: usize,
    resp_len: usize,
) -> &'static str {
    if req_len >= LARGE_BODY_THRESHOLD_BYTES
        || resp_len >= LARGE_BODY_THRESHOLD_BYTES
    {
        "s3"
    } else {
        "clickhouse"
    }
}

#[must_use]
pub fn inline_body_for_log(body: &Bytes) -> String {
    String::from_utf8_lossy(body.as_ref()).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_location_boundary_below_threshold() {
        assert_eq!(
            storage_location_for_sizes(LARGE_BODY_THRESHOLD_BYTES - 1, 0),
            "clickhouse"
        );
    }

    #[test]
    fn storage_location_boundary_at_threshold() {
        assert_eq!(
            storage_location_for_sizes(LARGE_BODY_THRESHOLD_BYTES, 0),
            "s3"
        );
    }

    #[test]
    fn clamp_body_ttl_days_zero_to_one() {
        assert_eq!(clamp_body_ttl_days(0), 1);
    }

    #[test]
    fn clamp_body_ttl_days_731_to_730() {
        assert_eq!(clamp_body_ttl_days(731), 730);
    }
}
