//! Reads and validates `MASTER_KEY_ENCRYPTION_KEY` from the environment.
//!
//! The key must be a standard Base64-encoded string that decodes to exactly 32
//! bytes (AES-256).

use base64::{Engine as _, engine::general_purpose::STANDARD as B64};

use crate::{crypto::master_key::MASTER_KEY_ENCRYPTION_KEY_LEN, error::init::InitError};

/// The environment variable name for the master key encryption key.
pub const MASTER_KEY_ENCRYPTION_KEY_ENV: &str = "MASTER_KEY_ENCRYPTION_KEY";

/// Reads `MASTER_KEY_ENCRYPTION_KEY` from the environment, Base64-decodes it
/// and validates that it is exactly 32 bytes. Returns an
/// `Arc<[u8; 32]>` suitable for sharing across tasks.
pub fn load_master_key_encryption_key() -> Result<std::sync::Arc<[u8; 32]>, InitError> {
    let raw = std::env::var(MASTER_KEY_ENCRYPTION_KEY_ENV).map_err(|_| {
        InitError::InvalidMasterKeyEncryptionKey(format!(
            "environment variable `{MASTER_KEY_ENCRYPTION_KEY_ENV}` is not set"
        ))
    })?;
    if raw.trim().is_empty() {
        return Err(InitError::InvalidMasterKeyEncryptionKey(format!(
            "`{MASTER_KEY_ENCRYPTION_KEY_ENV}` is set but empty"
        )));
    }
    let bytes = B64.decode(raw.trim()).map_err(|e| {
        InitError::InvalidMasterKeyEncryptionKey(format!("Base64 decode failed: {e}"))
    })?;
    if bytes.len() != MASTER_KEY_ENCRYPTION_KEY_LEN {
        return Err(InitError::InvalidMasterKeyEncryptionKey(format!(
            "key must be {MASTER_KEY_ENCRYPTION_KEY_LEN} bytes after decode, \
             got {}",
            bytes.len()
        )));
    }
    let mut arr = [0u8; MASTER_KEY_ENCRYPTION_KEY_LEN];
    arr.copy_from_slice(&bytes);
    Ok(std::sync::Arc::new(arr))
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, OnceLock};

    use base64::{Engine as _, engine::general_purpose::STANDARD as B64};

    use super::*;

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn with_master_key_env<T>(val: Option<&str>, f: impl FnOnce() -> T) -> T {
        // Tests mutate process-wide env vars; serialize and restore previous
        // value.
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous = std::env::var(MASTER_KEY_ENCRYPTION_KEY_ENV).ok();
        match val {
            Some(v) => unsafe {
                std::env::set_var(MASTER_KEY_ENCRYPTION_KEY_ENV, v);
            },
            None => unsafe {
                std::env::remove_var(MASTER_KEY_ENCRYPTION_KEY_ENV);
            },
        }

        let out = f();

        match previous {
            Some(v) => unsafe {
                std::env::set_var(MASTER_KEY_ENCRYPTION_KEY_ENV, v);
            },
            None => unsafe {
                std::env::remove_var(MASTER_KEY_ENCRYPTION_KEY_ENV);
            },
        }
        out
    }

    #[test]
    fn rejects_missing_key() {
        let err = with_master_key_env(None, || load_master_key_encryption_key().unwrap_err());
        assert!(err.to_string().contains("not set"));
    }

    #[test]
    fn rejects_empty_key() {
        let err = with_master_key_env(Some("  "), || load_master_key_encryption_key().unwrap_err());
        assert!(err.to_string().contains("empty"));
    }

    #[test]
    fn rejects_invalid_base64() {
        let err = with_master_key_env(Some("not-valid-base64!!!!"), || {
            load_master_key_encryption_key().unwrap_err()
        });
        assert!(err.to_string().to_lowercase().contains("base64"));
    }

    #[test]
    fn rejects_wrong_decoded_length() {
        // 16 bytes → too short
        let short = B64.encode([0u8; 16]);
        let err = with_master_key_env(Some(&short), || {
            load_master_key_encryption_key().unwrap_err()
        });
        assert!(err.to_string().contains("32 bytes"));
    }

    #[test]
    fn rejects_too_long_decoded_length() {
        let long = B64.encode([0u8; 33]);
        let err = with_master_key_env(Some(&long), || {
            load_master_key_encryption_key().unwrap_err()
        });
        assert!(err.to_string().contains("32 bytes"));
    }

    #[test]
    fn accepts_valid_32_byte_key() {
        let key_bytes = [42u8; 32];
        let encoded = B64.encode(key_bytes);
        let key = with_master_key_env(Some(&encoded), || load_master_key_encryption_key().unwrap());
        assert_eq!(key.as_slice(), &key_bytes);
    }

    #[test]
    fn accepts_valid_key_with_surrounding_whitespace() {
        let key_bytes = [7u8; 32];
        let encoded = B64.encode(key_bytes);
        let wrapped = format!("  \n{encoded}\t ");
        let key = with_master_key_env(Some(&wrapped), || load_master_key_encryption_key().unwrap());
        assert_eq!(key.as_slice(), &key_bytes);
    }
}
