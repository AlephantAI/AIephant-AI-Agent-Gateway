//! In-process sliding 1s window (monotonic clock).

use std::{
    collections::VecDeque,
    net::IpAddr,
    sync::Mutex,
    time::{Duration, Instant},
};

use rustc_hash::FxHashMap;

/// Maximum number of distinct IP entries tracked (prevents unbounded map
/// growth).
const MAX_TRACKED_IPS: usize = 100_000;

pub struct MemoryLimiter {
    limit: u32,
    inner: Mutex<MemoryInner>,
}

struct MemoryInner {
    map: FxHashMap<IpAddr, VecDeque<Instant>>,
}

impl MemoryLimiter {
    #[must_use]
    pub fn new(limit: u32) -> Self {
        Self {
            limit,
            inner: Mutex::new(MemoryInner {
                map: FxHashMap::default(),
            }),
        }
    }

    /// `Ok(())` allows, `Err(())` rejects (rate limited).
    pub fn check(&self, ip: IpAddr) -> Result<(), ()> {
        if self.limit == 0 {
            return Ok(());
        }
        let now = Instant::now();
        let window = Duration::from_secs(1);
        let mut guard = self.inner.lock().expect("memory limiter mutex poisoned");
        if guard.map.len() >= MAX_TRACKED_IPS && !guard.map.contains_key(&ip) {
            // Simple eviction: remove any entry (approximately random from hash
            // iteration order).
            if let Some(k) = guard.map.keys().next().copied() {
                guard.map.remove(&k);
            }
        }
        let deque = guard.map.entry(ip).or_default();
        while deque
            .front()
            .is_some_and(|t| now.duration_since(*t) > window)
        {
            deque.pop_front();
        }
        let active = u32::try_from(deque.len()).unwrap_or(u32::MAX);
        if active >= self.limit {
            return Err(());
        }
        deque.push_back(now);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;

    #[test]
    fn burst_within_one_instant_hits_limit() {
        let lim = MemoryLimiter::new(3);
        let ip = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1));
        assert!(lim.check(ip).is_ok());
        assert!(lim.check(ip).is_ok());
        assert!(lim.check(ip).is_ok());
        assert!(lim.check(ip).is_err());
    }

    #[test]
    fn limit_one_allows_single_then_rejects() {
        let lim = MemoryLimiter::new(1);
        let ip = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 88));
        assert!(lim.check(ip).is_ok());
        assert!(lim.check(ip).is_err());
    }

    #[test]
    fn distinct_ips_have_independent_buckets() {
        let lim = MemoryLimiter::new(1);
        let a = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1));
        let b = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 2));
        assert!(lim.check(a).is_ok());
        assert!(lim.check(b).is_ok());
        assert!(lim.check(a).is_err());
        assert!(lim.check(b).is_err());
    }
}
