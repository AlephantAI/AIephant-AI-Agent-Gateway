//! Plugin loader for config-driven plugin instantiation.
//!
//! Plugins are loaded based on YAML configuration, supporting both
//! built-in and third-party plugins.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::builtins::{
    DataClassifier, DataClassifierConfig, SensitiveDataDetector, SensitiveDataDetectorConfig,
};
use super::{ResponseData, SecurityContext, SecurityError, SecurityPlugin, get_plugin};

/// Plugin loading configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PluginConfig {
    /// Plugin name.
    pub name: String,
    /// Whether the plugin is enabled.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Execution priority override.
    pub priority: Option<i32>,
    /// Plugin-specific configuration.
    #[serde(default)]
    pub config: Option<toml::Value>,
}

impl Eq for PluginConfig {}

/// Default value for enabled field.
const fn default_enabled() -> bool {
    true
}

/// Top-level security plugin configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SecurityPluginsConfig {
    /// List of plugins to load.
    #[serde(default)]
    pub plugins: Vec<PluginConfig>,
}

/// Loaded plugin instance with metadata.
#[derive(Debug)]
struct LoadedPlugin {
    /// The plugin instance.
    plugin: Arc<dyn SecurityPlugin>,
    /// Configured priority (may override plugin's default).
    priority: i32,
}

/// Plugin loader that manages plugin lifecycle.
///
/// # Example
///
/// ```rust,ignore
/// let loader = PluginLoader::from_config(&config.security.plugins)?;
/// let security_layer = loader.into_layer();
/// ```
#[derive(Debug)]
pub struct PluginLoader {
    plugins: Vec<LoadedPlugin>,
}

impl PluginLoader {
    /// Create a new loader from configuration.
    ///
    /// Returns an error if a referenced plugin cannot be found.
    pub fn from_config(config: &SecurityPluginsConfig) -> Result<Self, SecurityError> {
        let mut plugins = Vec::new();

        for plugin_config in &config.plugins {
            if !plugin_config.enabled {
                tracing::debug!(name = %plugin_config.name, "plugin disabled, skipping");
                continue;
            }

            let plugin = create_plugin(&plugin_config.name, plugin_config.config.as_ref())?;

            let priority = plugin_config.priority.unwrap_or_else(|| plugin.priority());

            plugins.push(LoadedPlugin { plugin, priority });
        }

        // Sort by priority (lower = first)
        plugins.sort_by_key(|p| p.priority);

        Ok(Self { plugins })
    }

    /// Check a request against all enabled plugins.
    ///
    /// Plugins are checked in priority order. Returns on first error.
    pub fn check_request(&self, ctx: &SecurityContext) -> Result<(), SecurityError> {
        for loaded in &self.plugins {
            loaded.plugin.check_request(ctx)?;
        }
        Ok(())
    }

    /// Mask a response through all enabled plugins.
    ///
    /// Plugins are applied in priority order.
    pub fn mask_response(&self, data: &mut ResponseData) -> Result<(), SecurityError> {
        for loaded in &self.plugins {
            loaded.plugin.mask_response(data)?;
        }
        Ok(())
    }

    /// Returns the number of loaded plugins.
    #[must_use]
    pub fn len(&self) -> usize {
        self.plugins.len()
    }

    /// Returns true if no plugins are loaded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }

    /// Returns names of all loaded plugins in priority order.
    pub fn plugin_names(&self) -> Vec<&'static str> {
        self.plugins.iter().map(|p| p.plugin.name()).collect()
    }
}

impl Default for PluginLoader {
    fn default() -> Self {
        Self {
            plugins: Vec::new(),
        }
    }
}

/// Create a plugin instance by name with optional configuration.
fn create_plugin(
    name: &str,
    config: Option<&toml::Value>,
) -> Result<Arc<dyn SecurityPlugin>, SecurityError> {
    match name {
        "noop" => Ok(Arc::new(crate::plugin::NoOpSecurityPlugin)),

        "sensitive_data_detector" => {
            let detector_config = match config {
                Some(toml::Value::Table(table)) if table.is_empty() => {
                    SensitiveDataDetectorConfig::default()
                }
                Some(c) => SensitiveDataDetectorConfig::try_from(c).map_err(|e| {
                    SecurityError::ConfigError(format!("sensitive_data_detector: {e}"))
                })?,
                None => SensitiveDataDetectorConfig::default(),
            };
            Ok(Arc::new(SensitiveDataDetector::with_config(
                detector_config,
            )))
        }

        "data_classifier" => {
            let classifier_config = match config {
                Some(toml::Value::Table(table)) if table.is_empty() => {
                    DataClassifierConfig::default()
                }
                Some(c) => DataClassifierConfig::try_from(c)
                    .map_err(|e| SecurityError::ConfigError(format!("data_classifier: {e}")))?,
                None => DataClassifierConfig::default(),
            };
            Ok(Arc::new(DataClassifier::with_config(classifier_config)))
        }

        _ => {
            // Try to get from registry (for third-party plugins)
            let boxed = get_plugin(name)
                .ok_or_else(|| SecurityError::ConfigError(format!("plugin not found: {name}")))?;
            Ok(Arc::from(boxed))
        }
    }
}

// ---------------------------------------------------------------------------
// Configuration deserialization helpers
// ---------------------------------------------------------------------------

impl TryFrom<&toml::Value> for SensitiveDataDetectorConfig {
    type Error = toml::de::Error;

    fn try_from(value: &toml::Value) -> Result<Self, Self::Error> {
        let mut config = SensitiveDataDetectorConfig::default();

        if let Some(fields) = value.get("fields").and_then(|v| v.as_array()) {
            config.fields = fields
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
        }

        if let Some(patterns) = value.get("patterns").and_then(|v| v.as_array()) {
            config.patterns = patterns
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
        }

        Ok(config)
    }
}

impl TryFrom<&toml::Value> for DataClassifierConfig {
    type Error = toml::de::Error;

    fn try_from(value: &toml::Value) -> Result<Self, Self::Error> {
        let mut config = DataClassifierConfig::default();

        if let Some(fields) = value.get("confidential_fields").and_then(|v| v.as_array()) {
            config.confidential_fields = fields
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
        }

        if let Some(min_level) = value.get("min_level").and_then(|v| v.as_str()) {
            config.min_level = match min_level {
                "public" => super::SensitivityLevel::Public,
                "sensitive" => super::SensitivityLevel::Sensitive,
                "confidential" => super::SensitivityLevel::Confidential,
                _ => super::SensitivityLevel::Public,
            };
        }

        Ok(config)
    }
}
