use std::fmt::Write;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::types::{org::OrgId, user::UserId};

/// Computes the hash of an API key for storage and lookup in the virtual-key
/// cache. Accepts either a raw key (`sk-...`) or `Bearer <key>` and always
/// hashes only the extracted key material.
#[must_use]
pub fn hash_key(key: &str) -> String {
    let key = key.strip_prefix("Bearer ").unwrap_or(key);
    let mut hasher = Sha256::new();
    hasher.update(key.as_bytes());
    let result = hasher.finalize();

    result
        .iter()
        .fold(String::with_capacity(result.len() * 2), |mut acc, &b| {
            let _ = write!(acc, "{b:02x}");
            acc
        })
}

#[derive(Serialize, Deserialize, Debug, Clone, sqlx::FromRow, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub struct Key {
    pub key_hash: String,
    pub owner_id: UserId,
    pub organization_id: OrgId,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_key() {
        let key = "sk-alephant-test-key";
        let hash1 = hash_key(key);
        let hash2 = hash_key(key);

        assert_eq!(hash1, hash2, "Hash should be deterministic");
        assert_eq!(hash1.len(), 64, "SHA-256 hash should be 64 hex characters");

        let different_key = "sk-alephant-different-key";
        let different_hash = hash_key(different_key);
        assert_ne!(
            hash1, different_hash,
            "Different keys should produce different hashes"
        );

        let expected_hash = "d29586a28b2205de59cdc2e693a67fa4d82ac5fab08d6baca069b7798ecfef1a";
        assert_eq!(
            hash_key("Bearer sk-alephant-test-key"),
            expected_hash,
            "Hash should match expected value"
        );
        assert_eq!(
            hash_key("sk-alephant-test-key"),
            expected_hash,
            "Raw key and Bearer key should hash identically"
        );
    }
}
