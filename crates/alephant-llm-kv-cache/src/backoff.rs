//! Exponential backoff for on-demand LLM KV backends (spec 2026-04-21).

/// Milliseconds until the next attempt, given consecutive failure count.
pub fn next_delay_ms(
    failures: u32,
    base_ms: u64,
    cap_ms: u64,
    max_shift: u32,
) -> u64 {
    let shift = failures.min(max_shift);
    let mult = 1u64.checked_shl(shift).unwrap_or(u64::MAX);
    let raw = base_ms.saturating_mul(mult);
    raw.min(cap_ms)
}

pub const DEFAULT_BACKOFF_BASE_MS: u64 = 200;
pub const DEFAULT_BACKOFF_CAP_MS: u64 = 30_000;
pub const DEFAULT_BACKOFF_MAX_SHIFT: u32 = 16;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delay_doubles_until_cap() {
        assert_eq!(next_delay_ms(0, 200, 30_000, 16), 200);
        assert_eq!(next_delay_ms(1, 200, 30_000, 16), 400);
        assert_eq!(next_delay_ms(2, 200, 30_000, 16), 800);
    }

    #[test]
    fn delay_respects_cap() {
        assert_eq!(next_delay_ms(100, 200, 30_000, 16), 30_000);
    }
}
