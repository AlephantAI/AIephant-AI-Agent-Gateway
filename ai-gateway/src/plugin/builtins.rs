//! Built-in security plugins.
//!
//! This module provides the default plugins that ship with ai-gateway.

use std::collections::HashSet;

use super::{ResponseData, SecurityContext, SecurityError, SecurityPlugin, SensitivityLevel};

// ---------------------------------------------------------------------------
// NoOp Plugin
// ---------------------------------------------------------------------------

/// A no-op security plugin that passes all requests through unchanged.
///
/// This is the default plugin when no other plugins are configured,
/// ensuring the security layer always has a valid implementation.
#[derive(Debug, Clone, Default)]
pub struct NoOpSecurityPlugin;

impl SecurityPlugin for NoOpSecurityPlugin {
    fn name(&self) -> &'static str {
        "noop"
    }

    fn check_request(&self, _ctx: &SecurityContext) -> Result<(), SecurityError> {
        Ok(())
    }

    fn mask_response(&self, _data: &mut ResponseData) -> Result<(), SecurityError> {
        Ok(())
    }

    fn priority(&self) -> i32 {
        i32::MAX
    }
}

// ---------------------------------------------------------------------------
// Sensitive Data Detector Plugin
// ---------------------------------------------------------------------------

/// Plugin configuration for sensitive data detection.
#[derive(Debug, Clone)]
pub struct SensitiveDataDetectorConfig {
    /// Field names to detect (case-insensitive matching).
    pub fields: Vec<String>,
    /// Additional regex patterns to detect.
    pub patterns: Vec<String>,
    /// Custom sensitivity levels per field.
    pub field_levels: HashSet<String>,
}

impl Default for SensitiveDataDetectorConfig {
    fn default() -> Self {
        Self {
            // Common sensitive field names
            fields: vec![
                "password".into(),
                "token".into(),
                "secret".into(),
                "api_key".into(),
                "apikey".into(),
                "phone".into(),
                "tel".into(),
                "mobile".into(),
                "email".into(),
                "id_card".into(),
                "idcard".into(),
                "passport".into(),
                "ssn".into(),
                "bank_account".into(),
                "bankaccount".into(),
                "credit_card".into(),
                "creditcard".into(),
                "card_number".into(),
            ],
            patterns: vec![],
            field_levels: HashSet::new(),
        }
    }
}

/// A plugin that detects sensitive data in requests and responses.
#[derive(Debug)]
pub struct SensitiveDataDetector {
    config: SensitiveDataDetectorConfig,
}

impl SensitiveDataDetector {
    /// Create a new detector with default configuration.
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: SensitiveDataDetectorConfig::default(),
        }
    }

    /// Create a detector with custom configuration.
    #[must_use]
    pub fn with_config(config: SensitiveDataDetectorConfig) -> Self {
        Self { config }
    }

    fn check_json_for_sensitive(&self, body: &[u8]) -> Result<(), SecurityError> {
        // Simple JSON key detection without external dependencies
        // This is a basic implementation - production could use serde_json
        let body_str = String::from_utf8_lossy(body);

        for field in &self.config.fields {
            let pattern = format!(r#""{}":"#, field.to_lowercase());

            // Check if field exists in JSON (case-insensitive)
            if body_str.to_lowercase().contains(&pattern) {
                let level = if self.config.field_levels.contains(field) {
                    SensitivityLevel::Confidential
                } else {
                    SensitivityLevel::default_for_field(field)
                };

                if level.must_not_persist() {
                    return Err(SecurityError::SensitiveDataDetected(format!(
                        "confidential field detected: {}",
                        field
                    )));
                }
            }
        }

        Ok(())
    }
}

impl Default for SensitiveDataDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl SecurityPlugin for SensitiveDataDetector {
    fn name(&self) -> &'static str {
        "sensitive_data_detector"
    }

    fn priority(&self) -> i32 {
        10 // High priority - runs early
    }

    fn check_request(&self, ctx: &SecurityContext) -> Result<(), SecurityError> {
        if ctx.request_body.is_empty() {
            return Ok(());
        }

        self.check_json_for_sensitive(&ctx.request_body)
    }

    fn mask_response(&self, data: &mut ResponseData) -> Result<(), SecurityError> {
        if data.body.is_empty() {
            return Ok(());
        }

        let masked = mask_sensitive_json(&data.body, &self.config.fields);
        data.body = masked.into();
        Ok(())
    }
}

/// Mask sensitive fields in JSON bytes.
fn mask_sensitive_json(body: &[u8], fields: &[String]) -> String {
    let mut result = String::from_utf8_lossy(body).to_string();

    for field in fields {
        result = mask_json_string_field(&result, field);
    }

    result
}

fn mask_json_string_field(input: &str, field: &str) -> String {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(input) else {
        return input.to_string();
    };
    mask_json_value(&mut value, field);
    serde_json::to_string(&value).unwrap_or_else(|_| input.to_string())
}

fn mask_json_value(value: &mut serde_json::Value, field: &str) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                if key.eq_ignore_ascii_case(field) {
                    *value = serde_json::Value::String("***MASKED***".to_string());
                } else {
                    mask_json_value(value, field);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                mask_json_value(item, field);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Data Classifier Plugin
// ---------------------------------------------------------------------------

/// Plugin configuration for data classification.
#[derive(Debug, Clone, Default)]
pub struct DataClassifierConfig {
    /// Minimum sensitivity level to trigger classification.
    pub min_level: SensitivityLevel,
    /// Fields explicitly marked as confidential.
    pub confidential_fields: Vec<String>,
}

/// A plugin that classifies data sensitivity levels.
#[derive(Debug)]
pub struct DataClassifier {
    config: DataClassifierConfig,
}

impl DataClassifier {
    /// Create a new classifier with default configuration.
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: DataClassifierConfig::default(),
        }
    }

    /// Create a classifier with custom configuration.
    #[must_use]
    pub fn with_config(config: DataClassifierConfig) -> Self {
        Self { config }
    }
}

impl Default for DataClassifier {
    fn default() -> Self {
        Self::new()
    }
}

impl SecurityPlugin for DataClassifier {
    fn name(&self) -> &'static str {
        "data_classifier"
    }

    fn priority(&self) -> i32 {
        20
    }

    fn check_request(&self, ctx: &SecurityContext) -> Result<(), SecurityError> {
        // Classification is informational - always passes
        let level = classify_request(ctx, &self.config);
        tracing::debug!(
            level = ?level,
            vk_id = %ctx.virtual_key_id,
            "request classified"
        );
        Ok(())
    }

    fn mask_response(&self, data: &mut ResponseData) -> Result<(), SecurityError> {
        // Auto-detect and flag sensitive responses
        if is_likely_sensitive(&data.body) {
            data.sensitive = true;
        }
        Ok(())
    }
}

/// Classify a request's sensitivity level.
fn classify_request(ctx: &SecurityContext, config: &DataClassifierConfig) -> SensitivityLevel {
    let body_str = String::from_utf8_lossy(&ctx.request_body);
    let lower = body_str.to_lowercase();

    for field in &config.confidential_fields {
        let field = field.to_lowercase();
        let compact_pattern = format!(r#""{}":"#, field);
        let spaced_pattern = format!(r#""{}": "#, field);
        if lower.contains(&compact_pattern) || lower.contains(&spaced_pattern) {
            return SensitivityLevel::Confidential;
        }
    }

    // Check for high-sensitivity patterns
    let high_sensitivity = ["password", "secret", "token", "api_key", "private_key"];
    let personal_info = ["phone", "email", "id_card", "ssn", "passport"];
    let financial = ["bank_account", "credit_card", "card_number"];

    for field in &high_sensitivity {
        let pattern = format!(r#""{}":""#, field);
        if lower.contains(&pattern) {
            return SensitivityLevel::Confidential;
        }
    }

    for field in &personal_info {
        let pattern = format!(r#""{}":""#, field);
        if lower.contains(&pattern) {
            return SensitivityLevel::Sensitive;
        }
    }

    for field in &financial {
        let pattern = format!(r#""{}":""#, field);
        if lower.contains(&pattern) {
            return SensitivityLevel::Confidential;
        }
    }

    let level = SensitivityLevel::Public;
    if level.requires_masking() || config.min_level == SensitivityLevel::Public {
        level
    } else {
        config.min_level
    }
}

/// Heuristically determine if response body contains sensitive data.
fn is_likely_sensitive(body: &[u8]) -> bool {
    let body_str = String::from_utf8_lossy(body);
    let lower = body_str.to_lowercase();

    // Check for common sensitive data indicators in responses
    let indicators = ["email", "phone", "address", "ssn", "account"];

    for indicator in &indicators {
        if lower.contains(indicator) {
            return true;
        }
    }

    false
}

// ---------------------------------------------------------------------------
// Module exports
// ---------------------------------------------------------------------------

/// The standard set of built-in plugins.
#[derive(Debug, Clone, Default)]
pub struct BuiltinPlugins;

impl BuiltinPlugins {
    /// Returns iterator over built-in plugin names and factories.
    pub fn iter(&self) -> impl Iterator<Item = (&'static str, fn() -> Box<dyn SecurityPlugin>)> {
        // Note: We can't return impl Trait with different types, so we use a different approach
        std::iter::empty()
    }
}
