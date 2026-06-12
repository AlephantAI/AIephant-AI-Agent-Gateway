#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SchemaValidationError {
    #[error("arguments must be a JSON object")]
    ArgumentsMustBeObject,
    #[error("required argument is missing: {0}")]
    RequiredMissing(String),
    #[error("argument has invalid type: {0}")]
    InvalidType(String),
}

pub fn validate_arguments(
    schema: &serde_json::Value,
    arguments: &serde_json::Value,
) -> Result<(), SchemaValidationError> {
    let Some(argument_object) = arguments.as_object() else {
        return Err(SchemaValidationError::ArgumentsMustBeObject);
    };

    if let Some(required) = schema.get("required").and_then(|value| value.as_array()) {
        for field in required.iter().filter_map(|value| value.as_str()) {
            if !argument_object.contains_key(field) {
                return Err(SchemaValidationError::RequiredMissing(field.to_string()));
            }
        }
    }

    let Some(properties) = schema.get("properties").and_then(|value| value.as_object()) else {
        return Ok(());
    };

    for (field, property_schema) in properties {
        let Some(value) = argument_object.get(field) else {
            continue;
        };
        let Some(type_name) = property_schema.get("type").and_then(|value| value.as_str()) else {
            continue;
        };

        if !matches_type(type_name, value) {
            return Err(SchemaValidationError::InvalidType(field.clone()));
        }
    }

    Ok(())
}

fn matches_type(type_name: &str, value: &serde_json::Value) -> bool {
    match type_name {
        "string" => value.is_string(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "object" => value.is_object(),
        "array" => value.is_array(),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::{SchemaValidationError, validate_arguments};

    #[test]
    fn validates_required_string_property() {
        let schema = serde_json::json!({
            "type": "object",
            "required": ["message"],
            "properties": {
                "message": { "type": "string" }
            }
        });
        let arguments = serde_json::json!({ "message": "hello" });

        validate_arguments(&schema, &arguments).expect("valid arguments");
    }

    #[test]
    fn rejects_non_object_arguments() {
        let schema = serde_json::json!({ "type": "object" });
        let arguments = serde_json::json!("hello");

        let err = validate_arguments(&schema, &arguments).expect_err("arguments must be object");

        assert_eq!(err, SchemaValidationError::ArgumentsMustBeObject);
    }

    #[test]
    fn rejects_invalid_property_type() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "count": { "type": "number" }
            }
        });
        let arguments = serde_json::json!({ "count": "1" });

        let err = validate_arguments(&schema, &arguments).expect_err("count must be number");

        assert_eq!(err, SchemaValidationError::InvalidType("count".into()));
    }

    #[test]
    fn rejects_missing_required_property() {
        let schema = serde_json::json!({
            "type": "object",
            "required": ["message"],
            "properties": {
                "message": { "type": "string" }
            }
        });
        let arguments = serde_json::json!({});

        let err = validate_arguments(&schema, &arguments).expect_err("message is required");

        assert_eq!(
            err,
            SchemaValidationError::RequiredMissing("message".into())
        );
    }
}
