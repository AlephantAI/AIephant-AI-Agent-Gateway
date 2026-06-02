use bytes::Bytes;
use displaydoc::Display;
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error, Display)]
pub enum BodySchemaValidationError {
    /// request body must be valid JSON: {0}
    InvalidJson(serde_json::Error),
    /// x402 endpoint body schema is invalid: {0}
    InvalidSchema(String),
    /// request body does not match x402 endpoint schema: {0}
    Mismatch(String),
}

#[must_use]
pub fn schema_is_empty(schema: &Value) -> bool {
    matches!(schema, Value::Null)
        || matches!(schema, Value::Object(map) if map.is_empty())
}

pub fn validate_body_against_schema(
    body: &Bytes,
    schema: &Value,
) -> Result<(), BodySchemaValidationError> {
    if body.is_empty() || schema_is_empty(schema) {
        return Ok(());
    }

    let body_json = serde_json::from_slice(body)
        .map_err(BodySchemaValidationError::InvalidJson)?;
    let validator = jsonschema::validator_for(schema).map_err(|error| {
        BodySchemaValidationError::InvalidSchema(error.to_string())
    })?;

    validator
        .validate(&body_json)
        .map_err(|error| BodySchemaValidationError::Mismatch(error.to_string()))
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use serde_json::json;

    use super::{BodySchemaValidationError, validate_body_against_schema};

    fn object_schema() -> serde_json::Value {
        json!({
            "type": "object",
            "required": ["model"],
            "properties": {
                "model": { "type": "string" },
                "mode": { "enum": ["chat", "completion"] }
            }
        })
    }

    #[test]
    fn valid_body_matches_schema() {
        let body = Bytes::from_static(br#"{"model":"gpt-4.1","mode":"chat"}"#);

        let result = validate_body_against_schema(&body, &object_schema());

        assert!(result.is_ok());
    }

    #[test]
    fn empty_body_skips_validation() {
        let schema = json!({
            "type": "object",
            "required": ["model"],
        });

        let result = validate_body_against_schema(&Bytes::new(), &schema);

        assert!(result.is_ok());
    }

    #[test]
    fn null_schema_skips_validation() {
        let body = Bytes::from_static(b"not json");

        let result =
            validate_body_against_schema(&body, &serde_json::Value::Null);

        assert!(result.is_ok());
    }

    #[test]
    fn empty_object_schema_skips_validation() {
        let body = Bytes::from_static(b"not json");
        let schema = json!({});

        let result = validate_body_against_schema(&body, &schema);

        assert!(result.is_ok());
    }

    #[test]
    fn invalid_json_returns_json_error() {
        let body = Bytes::from_static(b"not json");

        let result = validate_body_against_schema(&body, &object_schema());

        assert!(matches!(
            result,
            Err(BodySchemaValidationError::InvalidJson(_))
        ));
    }

    #[test]
    fn missing_required_field_returns_mismatch() {
        let body = Bytes::from_static(br#"{"mode":"chat"}"#);

        let result = validate_body_against_schema(&body, &object_schema());

        assert!(matches!(
            result,
            Err(BodySchemaValidationError::Mismatch(_))
        ));
    }

    #[test]
    fn enum_mismatch_returns_mismatch() {
        let body = Bytes::from_static(br#"{"model":"gpt-4.1","mode":"audio"}"#);

        let result = validate_body_against_schema(&body, &object_schema());

        assert!(matches!(
            result,
            Err(BodySchemaValidationError::Mismatch(_))
        ));
    }

    #[test]
    fn invalid_schema_returns_schema_error() {
        let body = Bytes::from_static(br#"{"model":"gpt-4.1"}"#);
        let schema = json!({
            "type": 7,
        });

        let result = validate_body_against_schema(&body, &schema);

        assert!(matches!(
            result,
            Err(BodySchemaValidationError::InvalidSchema(_))
        ));
    }
}
