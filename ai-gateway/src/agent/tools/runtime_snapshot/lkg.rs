use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};

use super::types::SnapshotEnvelope;

#[derive(Debug, Clone)]
pub struct LkgSnapshotCache {
    ttl: Duration,
    snapshots: Arc<RwLock<HashMap<String, LkgCacheEntry>>>,
}

#[derive(Debug, Clone)]
struct LkgCacheEntry {
    loaded_at: Instant,
    envelope: SnapshotEnvelope,
}

impl LkgSnapshotCache {
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            snapshots: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn get(&self, workspace_id: &str) -> Option<SnapshotEnvelope> {
        let entry = self
            .snapshots
            .read()
            .expect("lkg snapshot cache read lock")
            .get(workspace_id)
            .cloned()?;

        (entry.loaded_at.elapsed() < self.ttl).then_some(entry.envelope)
    }

    pub fn put(&self, envelope: SnapshotEnvelope) {
        self.snapshots
            .write()
            .expect("lkg snapshot cache write lock")
            .insert(
                envelope.workspace_id.clone(),
                LkgCacheEntry {
                    loaded_at: Instant::now(),
                    envelope,
                },
            );
    }
}

impl Default for LkgSnapshotCache {
    fn default() -> Self {
        Self::new(Duration::from_secs(60))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::tools::runtime_snapshot::types::{RuntimeSnapshot, SnapshotSource};

    #[test]
    fn lkg_returns_snapshot_for_same_workspace() {
        let cache = LkgSnapshotCache::new(Duration::from_secs(60));
        cache.put(SnapshotEnvelope {
            workspace_id: "workspace-1".to_string(),
            snapshot: RuntimeSnapshot {
                workspace_id: "workspace-1".to_string(),
                snapshot_revision: 42,
                source: SnapshotSource::Static,
                ..RuntimeSnapshot::default()
            },
            ..SnapshotEnvelope::default()
        });

        let envelope = cache.get("workspace-1").expect("snapshot should be cached");

        assert_eq!(envelope.snapshot.workspace_id, "workspace-1");
        assert_eq!(envelope.snapshot.snapshot_revision, 42);
        assert!(cache.get("workspace-2").is_none());
    }

    #[test]
    fn lkg_returns_none_after_ttl_expires() {
        let cache = LkgSnapshotCache::new(Duration::from_millis(0));
        cache.put(SnapshotEnvelope {
            workspace_id: "workspace-1".to_string(),
            snapshot: RuntimeSnapshot {
                workspace_id: "workspace-1".to_string(),
                snapshot_revision: 42,
                source: SnapshotSource::Static,
                ..RuntimeSnapshot::default()
            },
            ..SnapshotEnvelope::default()
        });

        assert!(cache.get("workspace-1").is_none());
    }
}
