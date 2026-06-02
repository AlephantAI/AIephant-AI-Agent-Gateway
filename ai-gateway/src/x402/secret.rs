use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use thiserror::Error;

use crate::crypto::{
    master_key::{self, MASTER_KEY_NONCE_LEN},
    master_key_config,
};

const SECRET_CIPHERTEXT_PREFIX: &[u8] = b"v1:";

#[derive(Debug, Error)]
pub enum X402SecretDecryptError {
    #[error("invalid secret ciphertext prefix")]
    InvalidPrefix,
    #[error("invalid secret ciphertext base64: {0}")]
    InvalidBase64(#[from] base64::DecodeError),
    #[error("invalid ciphertext length: got {got}")]
    InvalidCiphertextLength { got: usize },
    #[error(transparent)]
    Decrypt(#[from] master_key::DecryptError),
    #[error(transparent)]
    MasterKeyConfig(#[from] crate::error::init::InitError),
}

impl PartialEq for X402SecretDecryptError {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::InvalidPrefix, Self::InvalidPrefix)
            | (Self::InvalidBase64(_), Self::InvalidBase64(_)) => true,
            (
                Self::InvalidCiphertextLength { got: got_a },
                Self::InvalidCiphertextLength { got: got_b },
            ) => got_a == got_b,
            (Self::Decrypt(a), Self::Decrypt(b)) => a == b,
            _ => false,
        }
    }
}

impl Eq for X402SecretDecryptError {}

pub fn decrypt_secret(
    secret_ciphertext: &[u8],
    key: &[u8],
) -> Result<Vec<u8>, X402SecretDecryptError> {
    let packed = secret_ciphertext
        .strip_prefix(SECRET_CIPHERTEXT_PREFIX)
        .ok_or(X402SecretDecryptError::InvalidPrefix)?;
    let packed = B64.decode(packed)?;
    if packed.len() < MASTER_KEY_NONCE_LEN {
        return Err(X402SecretDecryptError::InvalidCiphertextLength {
            got: packed.len(),
        });
    }

    let (nonce, ciphertext) = packed.split_at(MASTER_KEY_NONCE_LEN);
    Ok(master_key::decrypt(ciphertext, nonce, key)?)
}

pub fn decrypt_secret_from_env(
    secret_ciphertext: &[u8],
) -> Result<Vec<u8>, X402SecretDecryptError> {
    let key = master_key_config::load_master_key_encryption_key()?;
    decrypt_secret(secret_ciphertext, key.as_slice())
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, OnceLock};

    use base64::{Engine as _, engine::general_purpose::STANDARD as B64};

    use super::*;

    fn sample_key() -> [u8; 32] {
        *b"0123456789abcdef0123456789abcdef"
    }

    fn pack_secret(plaintext: &[u8], key: &[u8]) -> String {
        let (ciphertext, nonce) =
            crate::crypto::master_key::encrypt(plaintext, key).unwrap();
        let mut packed = nonce;
        packed.extend_from_slice(&ciphertext);
        format!("v1:{}", B64.encode(packed))
    }

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn with_master_key_env<T>(value: &str, f: impl FnOnce() -> T) -> T {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let env_name =
            crate::crypto::master_key_config::MASTER_KEY_ENCRYPTION_KEY_ENV;
        let previous = std::env::var(env_name).ok();
        unsafe {
            std::env::set_var(env_name, value);
        }

        let out = f();

        match previous {
            Some(value) => unsafe {
                std::env::set_var(env_name, value);
            },
            None => unsafe {
                std::env::remove_var(env_name);
            },
        }
        out
    }

    #[test]
    fn decrypt_secret_unpacks_v1_base64_nonce_and_ciphertext() {
        let key = sample_key();
        let packed = pack_secret(b"sk-x402-secret", &key);

        let plaintext = decrypt_secret(packed.as_bytes(), &key).unwrap();

        assert_eq!(plaintext, b"sk-x402-secret");
    }

    #[test]
    fn decrypt_secret_rejects_invalid_prefix() {
        let key = sample_key();

        let err = decrypt_secret(b"v2:abcd", &key).unwrap_err();

        assert_eq!(err, X402SecretDecryptError::InvalidPrefix);
    }

    #[test]
    fn decrypt_secret_rejects_invalid_base64() {
        let key = sample_key();

        let err = decrypt_secret(b"v1:not valid base64", &key).unwrap_err();

        assert!(matches!(err, X402SecretDecryptError::InvalidBase64(_)));
    }

    #[test]
    fn decrypt_secret_rejects_short_packed_payload() {
        let key = sample_key();
        let packed = format!("v1:{}", B64.encode([1u8; 11]));

        let err = decrypt_secret(packed.as_bytes(), &key).unwrap_err();

        assert_eq!(
            err,
            X402SecretDecryptError::InvalidCiphertextLength { got: 11 }
        );
    }

    #[test]
    fn decrypt_secret_from_env_uses_master_key_encryption_key() {
        let key = sample_key();
        let packed = pack_secret(b"from-env", &key);
        let encoded_key = B64.encode(key);

        let plaintext = with_master_key_env(&encoded_key, || {
            decrypt_secret_from_env(packed.as_bytes()).unwrap()
        });

        assert_eq!(plaintext, b"from-env");
    }
}
