//! Security plugin configuration.

use serde::{Deserialize, Serialize};

use crate::plugin::loader::SecurityPluginsConfig;

/// Security plugin configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct SecurityPluginConfiguration {
    /// Enable security plugins.
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// Plugin configurations.
    #[serde(default)]
    pub plugins: Vec<crate::plugin::loader::PluginConfig>,
}

const fn default_enabled() -> bool {
    true
}

impl SecurityPluginConfiguration {
    /// Convert to plugin loader config if enabled.
    pub fn to_loader_config(&self) -> Option<SecurityPluginsConfig> {
        if !self.enabled {
            return None;
        }
        Some(SecurityPluginsConfig {
            plugins: self.plugins.clone(),
        })
    }
}
