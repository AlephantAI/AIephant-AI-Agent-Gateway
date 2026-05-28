use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Controls how the dispatcher resolves master keys in Cloud mode.
///
/// Default for Cloud: `PrimaryThenWorkspaceFallback`.
#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Hash,
)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub enum MasterKeyResolution {
    /// Only use the master key directly linked to the virtual key.
    PrimaryOnly,
    /// Try the primary master key first; if unavailable, randomly pick
    /// another active master key in the same workspace and provider.
    #[default]
    PrimaryThenWorkspaceFallback,
}

impl MasterKeyResolution {
    /// Returns `true` if workspace-level fallback is enabled.
    #[must_use]
    pub fn fallback_enabled(self) -> bool {
        matches!(self, MasterKeyResolution::PrimaryThenWorkspaceFallback)
    }
}

#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Hash,
)]
#[serde(rename_all = "kebab-case")]
enum DeploymentTargetType {
    #[default]
    Cloud,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, Hash)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct DeploymentTarget {
    #[serde(default, rename = "type", skip_serializing)]
    _deprecated_type: DeploymentTargetType,
    #[serde(
        with = "humantime_serde",
        default = "default_db_poll_interval",
        rename = "db-poll-interval"
    )]
    pub db_poll_interval: Duration,
    #[serde(
        with = "humantime_serde",
        default = "default_listener_reconnect_interval",
        rename = "listener-reconnect-interval"
    )]
    pub listener_reconnect_interval: Duration,
    #[serde(default, rename = "master-key-resolution")]
    pub master_key_resolution: MasterKeyResolution,
}

impl Default for DeploymentTarget {
    fn default() -> Self {
        Self {
            _deprecated_type: DeploymentTargetType::Cloud,
            db_poll_interval: default_db_poll_interval(),
            listener_reconnect_interval: default_listener_reconnect_interval(),
            master_key_resolution: MasterKeyResolution::default(),
        }
    }
}

impl DeploymentTarget {
    #[must_use]
    pub fn new(
        db_poll_interval: Duration,
        listener_reconnect_interval: Duration,
        master_key_resolution: MasterKeyResolution,
    ) -> Self {
        Self {
            _deprecated_type: DeploymentTargetType::Cloud,
            db_poll_interval,
            listener_reconnect_interval,
            master_key_resolution,
        }
    }

    /// Returns `true` if workspace-level master key fallback is enabled.
    #[must_use]
    pub fn master_key_fallback_enabled(&self) -> bool {
        self.master_key_resolution.fallback_enabled()
    }

    #[must_use]
    pub fn log_label(&self) -> &'static str {
        "cloud"
    }
}

fn default_db_poll_interval() -> Duration {
    Duration::from_secs(30)
}

fn default_listener_reconnect_interval() -> Duration {
    // 5 minutes
    Duration::from_secs(300)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deployment_target_accepts_legacy_cloud_type_field() {
        let json = r#"{
          "type": "cloud",
          "db-poll-interval": "30s",
          "listener-reconnect-interval": "5m"
        }"#;

        let target: DeploymentTarget =
            serde_json::from_str(json).expect("deserialize cloud target");

        assert_eq!(target.db_poll_interval, Duration::from_secs(30));
        assert_eq!(
            target.listener_reconnect_interval,
            Duration::from_secs(300)
        );
        assert!(target.master_key_fallback_enabled());
    }

    #[test]
    fn deployment_target_omits_legacy_type_field_when_serialized() {
        let serialized = serde_json::to_string(&DeploymentTarget::default())
            .expect("serialize deployment target");
        assert!(!serialized.contains("\"type\""));
    }

    #[test]
    fn deployment_target_primary_only_disables_master_key_fallback() {
        let target = DeploymentTarget::new(
            Duration::from_secs(30),
            Duration::from_secs(300),
            MasterKeyResolution::PrimaryOnly,
        );
        assert!(!target.master_key_fallback_enabled());
    }
}
