//! Plugin system for security and data handling extensions.
//!
//! # Overview
//!
//! The plugin system allows runtime extension of security behaviors without
//! modifying core gateway code. Plugins are loaded via configuration and
//! executed in priority order.
//!
//! # Core Traits
//!
//! - [`SecurityPlugin`]: Main trait for security extensions
//! - [`PluginConfig`]: Plugin-specific configuration
//!
//! # Registry
//!
//! Plugins are registered via the global [`PLUGIN_REGISTRY`], which supports
//! both built-in and third-party plugins.
//!
//! # Configuration
//!
//! Security plugins are configured under `global.security` or `unified_api.security`:
//!
//! ```yaml
//! # In config.yaml - either global or unified_api section
//! global:
//!   security:
//!     enabled: true
//!     plugins:
//!       - name: "sensitive_data_detector"
//!         enabled: true
//!         priority: 10
//!
//! unified_api:
//!   security:
//!     enabled: true
//!     plugins:
//!       - name: "sensitive_data_detector"
//!         enabled: true
//!         priority: 10
//! ```
//!
//! Third-party plugins can use the [`register_plugin`] macro.

use bytes::Bytes;
use std::sync::Arc;

pub mod builtins;
pub mod loader;

pub use builtins::NoOpSecurityPlugin;
pub use loader::PluginLoader;

/// Security context passed to plugin check methods.
#[derive(Debug, Clone)]
pub struct SecurityContext {
    /// The virtual key ID making the request.
    pub virtual_key_id: String,
    /// Target provider ID.
    pub provider: String,
    /// Request body bytes.
    pub request_body: Bytes,
    /// Associated workspace ID (if any).
    pub workspace_id: Option<String>,
}

/// Response data that can be masked by plugins.
#[derive(Debug)]
pub struct ResponseData {
    /// Response body bytes.
    pub body: Bytes,
    /// Whether the response contains sensitive data.
    pub sensitive: bool,
}

/// Security plugin error types.
#[derive(Debug, thiserror::Error)]
pub enum SecurityError {
    #[error("sensitive data detected: {0}")]
    SensitiveDataDetected(String),

    #[error("access denied: {0}")]
    AccessDenied(String),

    #[error("plugin configuration error: {0}")]
    ConfigError(String),

    #[error("internal plugin error: {0}")]
    InternalError(String),
}

/// Data sensitivity classification levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum SensitivityLevel {
    /// Public data, can be logged without restriction.
    #[default]
    Public,
    /// Sensitive data, requires masking in logs.
    Sensitive,
    /// Confidential data, must not be persisted.
    Confidential,
}

impl SensitivityLevel {
    /// Returns true if this level requires masking.
    pub fn requires_masking(self) -> bool {
        matches!(
            self,
            SensitivityLevel::Sensitive | SensitivityLevel::Confidential
        )
    }

    /// Returns true if this level must not be persisted.
    pub fn must_not_persist(self) -> bool {
        matches!(self, SensitivityLevel::Confidential)
    }

    /// Returns the default sensitivity level for a given field name.
    pub fn default_for_field(field: &str) -> SensitivityLevel {
        let lower = field.to_lowercase();
        match lower.as_str() {
            "password" | "token" | "secret" | "api_key" | "apikey" | "private_key" => {
                SensitivityLevel::Confidential
            }
            "phone" | "tel" | "mobile" | "email" | "id_card" | "idcard" | "passport" | "ssn" => {
                SensitivityLevel::Sensitive
            }
            "bank_account" | "bankaccount" | "credit_card" | "creditcard" | "card_number" => {
                SensitivityLevel::Confidential
            }
            "medical" | "health" | "legal" | "court" => SensitivityLevel::Sensitive,
            _ => SensitivityLevel::Public,
        }
    }
}

/// Core trait for security plugins.
///
/// Implement this trait to create custom security behaviors such as:
/// - Sensitive data detection
/// - Response masking
/// - Access control
/// - Audit logging
///
/// # Example
///
/// ```rust
/// use ai_gateway::plugin::{SecurityPlugin, SecurityContext, ResponseData, SecurityError};
///
/// struct MyPlugin;
///
/// impl SecurityPlugin for MyPlugin {
///     fn name(&self) -> &'static str { "my_plugin" }
///     fn priority(&self) -> i32 { 50 }
///
///     fn check_request(&self, ctx: &SecurityContext) -> Result<(), SecurityError> {
///         // Custom check logic
///         Ok(())
///     }
///
///     fn mask_response(&self, data: &mut ResponseData) -> Result<(), SecurityError> {
///         // Custom masking logic
///         Ok(())
///     }
/// }
/// ```
pub trait SecurityPlugin: Send + Sync + std::fmt::Debug {
    /// Check if a request is allowed to proceed.
    ///
    /// This is called before the request is forwarded to the provider.
    /// Return `Ok(())` to allow, or `Err(SecurityError)` to deny.
    fn check_request(&self, ctx: &SecurityContext) -> Result<(), SecurityError>;

    /// Mask sensitive data in a response.
    ///
    /// This is called after receiving a response from the provider,
    /// before returning to the client.
    fn mask_response(&self, data: &mut ResponseData) -> Result<(), SecurityError>;

    /// Plugin identifier name (used in logs and metrics).
    fn name(&self) -> &'static str;

    /// Execution priority. Lower numbers execute first.
    ///
    /// Recommended ranges:
    /// - 1-50: Critical security (e.g., access control)
    /// - 51-100: Data protection (e.g., sensitive data detection)
    /// - 101+: Audit and logging
    fn priority(&self) -> i32 {
        100
    }
}

impl dyn SecurityPlugin {
    /// Returns the sensitivity level for a given field name.
    ///
    /// This is a utility method plugins can use to classify fields.
    pub fn default_sensitivity_for_field(field: &str) -> SensitivityLevel {
        let lower = field.to_lowercase();
        match lower.as_str() {
            // Authentication fields - highest sensitivity
            "password" | "token" | "secret" | "api_key" | "apikey" | "private_key" => {
                SensitivityLevel::Confidential
            }
            // Personal identifiable information
            "phone" | "tel" | "mobile" | "email" | "id_card" | "idcard" | "passport" | "ssn" => {
                SensitivityLevel::Sensitive
            }
            // Financial data
            "bank_account" | "bankaccount" | "credit_card" | "creditcard" | "card_number" => {
                SensitivityLevel::Confidential
            }
            // Medical and legal
            "medical" | "health" | "legal" | "court" => SensitivityLevel::Sensitive,
            // Default for unknown fields
            _ => SensitivityLevel::Public,
        }
    }
}

// ---------------------------------------------------------------------------
// Plugin Registry
// ---------------------------------------------------------------------------

use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::Mutex;

/// Global plugin factory registry.
///
/// Third-party plugins register themselves here via [`register_plugin`].
static PLUGIN_REGISTRY: Lazy<Arc<Mutex<HashMap<&'static str, fn() -> Box<dyn SecurityPlugin>>>>> =
    Lazy::new(|| Arc::new(Mutex::new(HashMap::new())));

/// Register a plugin factory with the global registry.
///
/// # Safety
///
/// This function is intended to be called during plugin initialization,
/// typically in a `#[no_mangle] extern "C"` function or in a static initializer.
/// It is not thread-safe to call this after the plugin system is running.
pub fn register_plugin(name: &'static str, factory: fn() -> Box<dyn SecurityPlugin>) {
    let mut registry = PLUGIN_REGISTRY.lock().unwrap();
    registry.insert(name, factory);
}

/// Get a registered plugin by name.
pub fn get_plugin(name: &str) -> Option<Box<dyn SecurityPlugin>> {
    let registry = PLUGIN_REGISTRY.lock().unwrap();
    registry.get(name).map(|factory| factory())
}
