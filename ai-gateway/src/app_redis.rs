//! **App-wide shared Redis** for `request_log.log_queue_redis_url` (process
//! singleton, lazy): request-log Stream, workspace concurrency counters, etc.

use tokio::sync::Mutex;
use url::Url;

#[derive(Debug)]
pub struct AppRedis {
    url: Url,
    conn: Mutex<Option<redis::aio::MultiplexedConnection>>,
}

impl AppRedis {
    #[must_use]
    pub fn new(url: Url) -> Self {
        Self {
            url,
            conn: Mutex::new(None),
        }
    }

    pub async fn ping(&self) -> Result<(), redis::RedisError> {
        let mut guard = self.conn.lock().await;
        if guard.is_none() {
            let client = redis::Client::open(self.url.as_str())?;
            *guard = Some(client.get_multiplexed_async_connection().await?);
        }
        let conn = guard.as_mut().expect("connection ensured");
        let _: redis::Value = redis::cmd("PING").query_async(conn).await?;
        Ok(())
    }

    pub async fn xadd_payload(
        &self,
        stream_key: &str,
        payload: &str,
    ) -> Result<(), redis::RedisError> {
        let mut guard = self.conn.lock().await;
        if guard.is_none() {
            let client = redis::Client::open(self.url.as_str())?;
            *guard = Some(client.get_multiplexed_async_connection().await?);
        }
        let conn = guard.as_mut().expect("connection ensured");
        redis::cmd("XADD")
            .arg(stream_key)
            .arg("*")
            .arg("payload")
            .arg(payload)
            .query_async::<redis::Value>(conn)
            .await?;
        Ok(())
    }

    pub async fn incr_and_expire(
        &self,
        key: &str,
        ttl_secs: i64,
    ) -> Result<(), redis::RedisError> {
        use redis::AsyncCommands;
        let mut guard = self.conn.lock().await;
        if guard.is_none() {
            let client = redis::Client::open(self.url.as_str())?;
            *guard = Some(client.get_multiplexed_async_connection().await?);
        }
        let conn = guard.as_mut().expect("connection ensured");
        let _: i64 = conn.incr(key, 1_i64).await?;
        let _: bool = conn.expire(key, ttl_secs).await?;
        Ok(())
    }

    pub async fn invoke_script(
        &self,
        script: &redis::Script,
        key: &str,
        arg: i64,
    ) -> Result<(), redis::RedisError> {
        let mut guard = self.conn.lock().await;
        if guard.is_none() {
            let client = redis::Client::open(self.url.as_str())?;
            *guard = Some(client.get_multiplexed_async_connection().await?);
        }
        let conn = guard.as_mut().expect("connection ensured");
        let _: redis::Value =
            script.key(key).arg(arg).invoke_async(conn).await?;
        Ok(())
    }

    /// Global sliding-window rate limit: `true` allow, `false` reject.
    /// `window_ms` is usually 1000.
    pub async fn client_ip_rate_limit_allow(
        &self,
        script: &redis::Script,
        key: &str,
        window_ms: i64,
        limit: i64,
        member: &str,
    ) -> Result<bool, redis::RedisError> {
        let mut guard = self.conn.lock().await;
        if guard.is_none() {
            let client = redis::Client::open(self.url.as_str())?;
            *guard = Some(client.get_multiplexed_async_connection().await?);
        }
        let conn = guard.as_mut().expect("connection ensured");
        let v: i64 = script
            .key(key)
            .arg(window_ms)
            .arg(limit)
            .arg(member)
            .invoke_async(conn)
            .await?;
        Ok(v == 1)
    }

    /// In-flight acquire: `true` reserved a slot; `false` means at limit (Redis
    /// rolled back).
    pub async fn gateway_in_flight_try_acquire(
        &self,
        script: &redis::Script,
        key: &str,
        ttl_secs: i64,
        max: i64,
    ) -> Result<bool, redis::RedisError> {
        let mut guard = self.conn.lock().await;
        if guard.is_none() {
            let client = redis::Client::open(self.url.as_str())?;
            *guard = Some(client.get_multiplexed_async_connection().await?);
        }
        let conn = guard.as_mut().expect("connection ensured");
        let v: i64 = script
            .key(key)
            .arg(ttl_secs)
            .arg(max)
            .invoke_async(conn)
            .await?;
        Ok(v == 1)
    }

    pub async fn get_opt_string(
        &self,
        key: &str,
    ) -> Result<Option<String>, redis::RedisError> {
        use redis::AsyncCommands;
        let mut guard = self.conn.lock().await;
        if guard.is_none() {
            let client = redis::Client::open(self.url.as_str())?;
            *guard = Some(client.get_multiplexed_async_connection().await?);
        }
        let conn = guard.as_mut().expect("connection ensured");
        let v: Option<String> = conn.get(key).await?;
        Ok(v)
    }

    pub async fn key_exists(
        &self,
        key: &str,
    ) -> Result<bool, redis::RedisError> {
        use redis::AsyncCommands;
        let mut guard = self.conn.lock().await;
        if guard.is_none() {
            let client = redis::Client::open(self.url.as_str())?;
            *guard = Some(client.get_multiplexed_async_connection().await?);
        }
        let conn = guard.as_mut().expect("connection ensured");
        conn.exists(key).await
    }

    pub async fn get_string(
        &self,
        key: &str,
    ) -> Result<Option<String>, redis::RedisError> {
        use redis::AsyncCommands;

        let mut guard = self.conn.lock().await;
        if guard.is_none() {
            let client = redis::Client::open(self.url.as_str())?;
            *guard = Some(client.get_multiplexed_async_connection().await?);
        }
        let conn = guard.as_mut().expect("connection ensured");
        conn.get(key).await
    }

    pub async fn set_ex(
        &self,
        key: &str,
        value: &str,
        ttl_secs: u64,
    ) -> Result<(), redis::RedisError> {
        use redis::AsyncCommands;
        let mut guard = self.conn.lock().await;
        if guard.is_none() {
            let client = redis::Client::open(self.url.as_str())?;
            *guard = Some(client.get_multiplexed_async_connection().await?);
        }
        let conn = guard.as_mut().expect("connection ensured");
        let _: () = conn.set_ex(key, value, ttl_secs).await?;
        Ok(())
    }

    pub async fn del(&self, key: &str) -> Result<(), redis::RedisError> {
        use redis::AsyncCommands;
        let mut guard = self.conn.lock().await;
        if guard.is_none() {
            let client = redis::Client::open(self.url.as_str())?;
            *guard = Some(client.get_multiplexed_async_connection().await?);
        }
        let conn = guard.as_mut().expect("connection ensured");
        let _: i64 = conn.del(key).await?;
        Ok(())
    }
}
