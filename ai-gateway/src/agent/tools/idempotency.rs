use sha2::{Digest, Sha256};

use crate::agent::tools::runtime_snapshot::types::VersionVector;

pub fn arguments_hash(arguments: &serde_json::Value) -> String {
    let bytes = serde_json::to_vec(arguments).expect("JSON value should serialize");
    sha256_hex(&bytes)
}

pub fn request_hash(
    tool_id: &str,
    idempotency_key: Option<&str>,
    vector: &VersionVector,
    arguments: &serde_json::Value,
) -> String {
    let payload = serde_json::json!({
        "toolId": tool_id,
        "idempotencyKey": idempotency_key,
        "versionVector": vector,
        "argumentsHash": arguments_hash(arguments),
    });
    let bytes = serde_json::to_vec(&payload).expect("hash payload should serialize");
    sha256_hex(&bytes)
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use crate::agent::tools::{
        idempotency::{arguments_hash, request_hash},
        runtime_snapshot::types::VersionVector,
    };

    #[test]
    fn request_hash_changes_when_arguments_change() {
        let vector = version_vector();

        let first = request_hash(
            "support.echo",
            Some("idem-1"),
            &vector,
            &serde_json::json!({ "message": "hello" }),
        );
        let second = request_hash(
            "support.echo",
            Some("idem-1"),
            &vector,
            &serde_json::json!({ "message": "goodbye" }),
        );

        assert_ne!(first, second);
    }

    #[test]
    fn request_hash_is_stable_for_same_input() {
        let vector = version_vector();
        let arguments = serde_json::json!({ "message": "hello" });

        let first = request_hash("support.echo", Some("idem-1"), &vector, &arguments);
        let second = request_hash("support.echo", Some("idem-1"), &vector, &arguments);

        assert_eq!(first, second);
        assert!(first.starts_with("sha256:"));
    }

    #[test]
    fn arguments_hash_is_stable_for_same_json_value() {
        let arguments = serde_json::json!({ "message": "hello" });

        assert_eq!(arguments_hash(&arguments), arguments_hash(&arguments));
    }

    fn version_vector() -> VersionVector {
        VersionVector {
            snapshot_revision: 42,
            active_pointer_revision: 7,
            payload_hash: "sha256:payload".to_string(),
            toolset_hash: "sha256:toolset".to_string(),
            policy_revision: 3,
            tool_id: "support.echo".to_string(),
            tool_version: 5,
            schema_hash: "sha256:schema".to_string(),
            rate_card_revision: 11,
            target_revision: 13,
        }
    }
}
