use std::time::Duration;

use futures::future::try_join_all;
use rand::seq::IndexedRandom as _;
use tokio::time::timeout;
use tracing::info;

use crate::{backend::LlmKvBackend, value::LlmCacheEntry};

/// Parallel read with 2s budget. Any backend error → `None` (MISS). Any empty
/// slot → `None`. All full → random slot.
pub async fn read_bucket(
    backend: &dyn LlmKvBackend,
    keys: &[String],
) -> Option<(LlmCacheEntry, usize)> {
    let fut = async {
        let futs: Vec<_> = keys.iter().map(|k| backend.get(k)).collect();
        let outs = try_join_all(futs).await.ok()?;
        let mut slots: Vec<Option<LlmCacheEntry>> =
            Vec::with_capacity(outs.len());
        for o in outs {
            match o {
                None => slots.push(None),
                Some(s) => match serde_json::from_str::<LlmCacheEntry>(&s) {
                    Ok(e) => slots.push(Some(e)),
                    Err(_) => slots.push(None),
                },
            }
        }
        if slots.iter().any(|s| s.is_none()) {
            return None;
        }
        let entries: Vec<LlmCacheEntry> = slots.into_iter().flatten().collect();
        let n = entries.len();
        let idx = *(0..n).collect::<Vec<_>>().choose(&mut rand::rng())?;
        Some((entries[idx].clone(), idx))
    };
    timeout(Duration::from_secs(2), fut).await.ok().flatten()
}

/// First parallel `get`; first `None` slot wins `put`. Any `get` error → no
/// write.
pub async fn try_save_to_first_free_slot(
    backend: &dyn LlmKvBackend,
    keys: &[String],
    entry: &LlmCacheEntry,
    ttl_secs: u64,
) -> Result<bool, crate::error::LlmKvCacheError> {
    info!(
        "[LLM KV try_save] start slots={} ttl={ttl_secs} body_chunks={}",
        keys.len(),
        entry.body.len()
    );
    for (i, k) in keys.iter().enumerate() {
        info!("[LLM KV try_save]   key[{i}]={k}");
    }

    let outs =
        futures::future::join_all(keys.iter().map(|k| backend.get(k))).await;
    for (i, r) in outs.iter().enumerate() {
        match r {
            Ok(None) => {
                info!("[LLM KV try_save]   probe[{i}]=miss (kv empty)");
            }
            Ok(Some(s)) => {
                info!(
                    "[LLM KV try_save]   probe[{i}]=occupied value_len={}",
                    s.len()
                );
            }
            Err(e) => {
                info!(
                    "[LLM KV try_save]   probe[{i}]=error err={e} -> abort \
                     save"
                );
                return Ok(false);
            }
        }
    }

    let slots: Vec<Option<String>> =
        outs.into_iter().map(|r| r.unwrap()).collect();
    let Some(free_idx) = slots.iter().position(std::option::Option::is_none)
    else {
        info!(
            "[LLM KV try_save] no free slot (all {} keys occupied) -> skip put",
            keys.len()
        );
        return Ok(false);
    };

    let json = match serde_json::to_string(entry) {
        Ok(j) => j,
        Err(e) => {
            info!("[LLM KV try_save] serialize LlmCacheEntry failed: {e}");
            return Err(e.into());
        }
    };

    info!(
        "[LLM KV try_save] put slot_idx={free_idx} key={} json_len={} \
         ttl={ttl_secs}",
        keys[free_idx],
        json.len()
    );
    match backend.put(&keys[free_idx], &json, ttl_secs).await {
        Ok(()) => {
            info!("[LLM KV try_save] put ok slot_idx={free_idx}");
            Ok(true)
        }
        Err(e) => {
            info!("[LLM KV try_save] put failed slot_idx={free_idx} err={e}");
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Mutex};

    use async_trait::async_trait;

    use super::*;

    struct MockBackend(Mutex<HashMap<String, String>>);

    impl MockBackend {
        fn new() -> Self {
            Self(Mutex::new(HashMap::new()))
        }
    }

    #[async_trait]
    impl LlmKvBackend for MockBackend {
        async fn get(
            &self,
            key: &str,
        ) -> Result<Option<String>, crate::error::LlmKvCacheError> {
            Ok(self.0.lock().expect("lock").get(key).cloned())
        }

        async fn put(
            &self,
            key: &str,
            value: &str,
            _expiration_ttl_secs: u64,
        ) -> Result<(), crate::error::LlmKvCacheError> {
            self.0
                .lock()
                .expect("lock")
                .insert(key.to_string(), value.to_string());
            Ok(())
        }
    }

    fn sample_entry() -> LlmCacheEntry {
        LlmCacheEntry {
            headers: HashMap::new(),
            latency: 1,
            body: vec!["a".into()],
        }
    }

    #[tokio::test]
    async fn read_miss_when_slot_empty() {
        let b = MockBackend::new();
        let keys = vec!["k0".into(), "k1".into()];
        let json = serde_json::to_string(&sample_entry()).unwrap();
        b.put("k0", &json, 60).await.unwrap();
        let r = read_bucket(&b, &keys).await;
        assert!(r.is_none());
    }

    #[tokio::test]
    async fn read_hit_when_all_full() {
        let b = MockBackend::new();
        let e = sample_entry();
        let json = serde_json::to_string(&e).unwrap();
        let keys = vec!["k0".into(), "k1".into()];
        b.put("k0", &json, 60).await.unwrap();
        b.put("k1", &json, 60).await.unwrap();
        let r = read_bucket(&b, &keys).await;
        assert!(r.is_some());
    }

    #[tokio::test]
    async fn save_first_free() {
        let b = MockBackend::new();
        let keys = vec!["k0".into(), "k1".into()];
        let e = sample_entry();
        let json = serde_json::to_string(&e).unwrap();
        b.put("k0", &json, 60).await.unwrap();
        let ok = try_save_to_first_free_slot(&b, &keys, &e, 60)
            .await
            .unwrap();
        assert!(ok);
        assert!(b.0.lock().expect("lock").contains_key("k1"));
    }
}
