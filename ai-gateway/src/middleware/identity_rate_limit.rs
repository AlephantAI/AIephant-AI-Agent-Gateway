use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

use crate::{
    app_redis::AppRedis,
    config::{
        client_ip_rate_limit::ClientIpRateLimitBackend,
        identity_rate_limit::IdentityRateLimitConfig,
    },
};
use uuid::Uuid;

#[derive(Debug, PartialEq, Eq)]
pub enum IdentityRateLimitError {
    Rejected { retry_after: u64 },
    Unavailable,
}

#[derive(Debug)]
struct Window {
    count: u64,
    expires: Instant,
}

#[derive(Debug, Default)]
struct MemoryState {
    windows: HashMap<Uuid, Window>,
    last_cleanup: Option<Instant>,
}

#[derive(Debug, Default)]
pub struct IdentityRateLimiter {
    memory: Mutex<MemoryState>,
}

impl IdentityRateLimiter {
    pub async fn check(
        &self,
        workspace: Uuid,
        config: &IdentityRateLimitConfig,
        redis: Option<&AppRedis>,
    ) -> Result<(), IdentityRateLimitError> {
        if !config.enabled {
            return Ok(());
        }
        match config.backend {
            ClientIpRateLimitBackend::Memory => {
                self.check_memory_at(workspace, config, Instant::now())
            }
            ClientIpRateLimitBackend::Redis => {
                let redis = redis.ok_or(IdentityRateLimitError::Unavailable)?;
                let key = format!("{}workspace:{workspace}", config.redis_key_prefix);
                let result = tokio::time::timeout(
                    Duration::from_secs(1),
                    redis.identity_rate_limit_count(&key, i64::from(config.window_seconds) * 1000),
                )
                .await;
                let (count, ttl, repaired) = result
                    .map_err(|_| IdentityRateLimitError::Unavailable)?
                    .map_err(|_| IdentityRateLimitError::Unavailable)?;
                if count < 1 || !(0..=86400_000).contains(&ttl) {
                    return Err(IdentityRateLimitError::Unavailable);
                }
                if repaired != 0 {
                    tracing::warn!(workspace_id = %workspace, "repaired identity rate limit expiry without resetting count");
                }
                if count > i64::from(config.requests) {
                    return Err(IdentityRateLimitError::Rejected {
                        retry_after: ((ttl + 999) / 1000).max(1) as u64,
                    });
                }
                Ok(())
            }
        }
    }

    fn check_memory_at(
        &self,
        workspace: Uuid,
        config: &IdentityRateLimitConfig,
        now: Instant,
    ) -> Result<(), IdentityRateLimitError> {
        let window = Duration::from_secs(u64::from(config.window_seconds));
        let mut memory = self
            .memory
            .lock()
            .map_err(|_| IdentityRateLimitError::Unavailable)?;
        if memory
            .last_cleanup
            .is_none_or(|last| now.saturating_duration_since(last) >= window)
        {
            memory.windows.retain(|_, value| value.expires > now);
            memory.last_cleanup = Some(now);
        }
        let state = memory.windows.entry(workspace).or_insert(Window {
            count: 0,
            expires: now + window,
        });
        if state.expires <= now {
            *state = Window {
                count: 0,
                expires: now + window,
            };
        }
        state.count = state.count.saturating_add(1);
        if state.count > u64::from(config.requests) {
            let left = state.expires.saturating_duration_since(now);
            return Err(IdentityRateLimitError::Rejected {
                retry_after: (left.as_secs() + u64::from(left.subsec_nanos() > 0)).max(1),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct RedisFixture {
        child: Option<std::process::Child>,
        url: url::Url,
    }

    impl RedisFixture {
        async fn start() -> Self {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            drop(listener);
            let child = std::process::Command::new("redis-server")
                .args([
                    "--bind",
                    "127.0.0.1",
                    "--port",
                    &port.to_string(),
                    "--save",
                    "",
                    "--appendonly",
                    "no",
                ])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("install redis-server to run this explicit backend test");
            let fixture = Self {
                child: Some(child),
                url: format!("redis://127.0.0.1:{port}").parse().unwrap(),
            };
            let probe = AppRedis::new(fixture.url.clone());
            for _ in 0..100 {
                if probe.ping().await.is_ok() {
                    return fixture;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            panic!("temporary Redis did not start");
        }

        fn stop(&mut self) {
            if let Some(mut child) = self.child.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    impl Drop for RedisFixture {
        fn drop(&mut self) {
            self.stop();
        }
    }

    #[tokio::test]
    #[ignore = "requires a local redis-server executable; run explicitly"]
    async fn redis_shares_atomic_counts_repairs_expiry_and_denies_after_backend_failure() {
        let mut fixture = RedisFixture::start().await;
        let left = AppRedis::new(fixture.url.clone());
        let right = AppRedis::new(fixture.url.clone());
        let one = IdentityRateLimiter::default();
        let two = IdentityRateLimiter::default();
        let config = IdentityRateLimitConfig {
            enabled: true,
            requests: 2,
            backend: ClientIpRateLimitBackend::Redis,
            ..Default::default()
        };
        let workspace = Uuid::from_u128(1);
        assert_eq!(one.check(workspace, &config, Some(&left)).await, Ok(()));
        assert_eq!(two.check(workspace, &config, Some(&right)).await, Ok(()));
        assert!(matches!(
            one.check(workspace, &config, Some(&left)).await,
            Err(IdentityRateLimitError::Rejected { .. })
        ));
        assert_eq!(
            two.check(Uuid::from_u128(2), &config, Some(&right)).await,
            Ok(())
        );
        let key = format!("{}workspace:{workspace}", config.redis_key_prefix);
        left.invoke_script(
            &redis::Script::new("return redis.call('SET', KEYS[1], ARGV[1])"),
            &key,
            2,
        )
        .await
        .unwrap();
        assert!(matches!(
            two.check(workspace, &config, Some(&right)).await,
            Err(IdentityRateLimitError::Rejected { .. })
        ));
        let (count, ttl, repaired) = left.identity_rate_limit_count(&key, 60_000).await.unwrap();
        assert_eq!(count, 4, "TTL repair must preserve the existing count");
        assert!((1..=60_000).contains(&ttl));
        assert_eq!(repaired, 0);
        left.invoke_script(
            &redis::Script::new("return redis.call('PEXPIRE', KEYS[1], ARGV[1])"),
            &key,
            1,
        )
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(one.check(workspace, &config, Some(&left)).await, Ok(()));
        fixture.stop();
        assert_eq!(
            one.check(Uuid::from_u128(3), &config, Some(&left)).await,
            Err(IdentityRateLimitError::Unavailable)
        );
    }

    #[test]
    fn fixed_window_counts_share_workspace_and_expire_without_extending_on_rejection() {
        let limiter = IdentityRateLimiter::default();
        let config = IdentityRateLimitConfig {
            enabled: true,
            requests: 1,
            window_seconds: 1,
            ..Default::default()
        };
        let start = Instant::now();
        assert_eq!(
            limiter.check_memory_at(Uuid::from_u128(1), &config, start),
            Ok(())
        );
        assert_eq!(
            limiter.check_memory_at(
                Uuid::from_u128(1),
                &config,
                start + Duration::from_millis(900)
            ),
            Err(IdentityRateLimitError::Rejected { retry_after: 1 })
        );
        assert_eq!(
            limiter.check_memory_at(Uuid::from_u128(2), &config, start),
            Ok(())
        );
        assert_eq!(
            limiter.check_memory_at(Uuid::from_u128(1), &config, start + Duration::from_secs(1)),
            Ok(())
        );
    }

    #[tokio::test]
    async fn redis_unavailable_is_not_silently_replaced_by_memory() {
        let limiter = IdentityRateLimiter::default();
        let config = IdentityRateLimitConfig {
            enabled: true,
            backend: ClientIpRateLimitBackend::Redis,
            ..Default::default()
        };
        assert_eq!(
            limiter.check(Uuid::from_u128(1), &config, None).await,
            Err(IdentityRateLimitError::Unavailable)
        );
    }

    #[test]
    fn maximum_configured_request_limit_does_not_wrap_and_allow_unlimited_requests() {
        let limiter = IdentityRateLimiter::default();
        let config = IdentityRateLimitConfig {
            enabled: true,
            requests: u32::MAX,
            ..Default::default()
        };
        let start = Instant::now();
        limiter.memory.lock().unwrap().windows.insert(
            Uuid::from_u128(1),
            Window {
                count: u64::from(u32::MAX),
                expires: start + Duration::from_secs(60),
            },
        );
        assert!(matches!(
            limiter.check_memory_at(Uuid::from_u128(1), &config, start),
            Err(IdentityRateLimitError::Rejected { .. })
        ));
    }
}
