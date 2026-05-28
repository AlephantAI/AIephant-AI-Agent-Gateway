//! AES-256-GCM for `master_keys.key_ciphertext` / `key_nonce` (compatible with
//! Go encrypt path).

use aes_gcm::{
    Aes256Gcm, Key, KeyInit, Nonce,
    aead::{Aead, AeadCore, OsRng},
};
use thiserror::Error;

/// Length of `MASTER_KEY_ENCRYPTION_KEY` after Base64 decode.
pub const MASTER_KEY_ENCRYPTION_KEY_LEN: usize = 32;
/// GCM nonce size (matches Go `gcm.NonceSize()`).
pub const MASTER_KEY_NONCE_LEN: usize = 12;

#[derive(Debug, Error, Clone, Eq, PartialEq)]
pub enum DecryptError {
    #[error(
        "encryption key must be {MASTER_KEY_ENCRYPTION_KEY_LEN} bytes, got {0}"
    )]
    InvalidKeyLen(usize),
    #[error("nonce must be {MASTER_KEY_NONCE_LEN} bytes, got {0}")]
    InvalidNonceLen(usize),
    #[error("decryption or authentication failed")]
    DecryptFailed,
}

#[derive(Debug, Error, Clone, Eq, PartialEq)]
pub enum EncryptError {
    #[error(
        "encryption key must be {MASTER_KEY_ENCRYPTION_KEY_LEN} bytes, got {0}"
    )]
    InvalidKeyLen(usize),
    #[error("encryption failed")]
    EncryptFailed,
}

/// Decrypt master key ciphertext using workspace encryption key (32 bytes).
pub fn decrypt(
    ciphertext: &[u8],
    nonce: &[u8],
    key: &[u8],
) -> Result<Vec<u8>, DecryptError> {
    if key.len() != MASTER_KEY_ENCRYPTION_KEY_LEN {
        return Err(DecryptError::InvalidKeyLen(key.len()));
    }
    if nonce.len() != MASTER_KEY_NONCE_LEN {
        return Err(DecryptError::InvalidNonceLen(nonce.len()));
    }
    let cipher_key = Key::<Aes256Gcm>::from_slice(key);
    let cipher = Aes256Gcm::new(cipher_key);
    let nonce = Nonce::from_slice(nonce);
    cipher
        .decrypt(nonce, ciphertext.as_ref())
        .map_err(|_| DecryptError::DecryptFailed)
}

/// Encrypt plaintext (for tests and tooling); returns `(ciphertext, nonce)`.
pub fn encrypt(
    plaintext: &[u8],
    key: &[u8],
) -> Result<(Vec<u8>, Vec<u8>), EncryptError> {
    if key.len() != MASTER_KEY_ENCRYPTION_KEY_LEN {
        return Err(EncryptError::InvalidKeyLen(key.len()));
    }
    let cipher_key = Key::<Aes256Gcm>::from_slice(key);
    let cipher = Aes256Gcm::new(cipher_key);
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ciphertext = cipher
        .encrypt(&nonce, plaintext.as_ref())
        .map_err(|_| EncryptError::EncryptFailed)?;
    Ok((ciphertext, nonce.to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_key() -> [u8; 32] {
        *b"0123456789abcdef0123456789abcdef"
    }

    #[test]
    fn decrypt_rejects_short_key() {
        let key = [0u8; 31];
        let nonce = [0u8; 12];
        assert_eq!(
            decrypt(&[], &nonce, &key).unwrap_err(),
            DecryptError::InvalidKeyLen(31)
        );
    }

    #[test]
    fn decrypt_rejects_wrong_nonce_len() {
        let key = sample_key();
        let nonce = [0u8; 11];
        assert_eq!(
            decrypt(&[], &nonce, &key).unwrap_err(),
            DecryptError::InvalidNonceLen(11)
        );
    }

    #[test]
    fn decrypt_rejects_garbage_ciphertext() {
        let key = sample_key();
        let nonce = [1u8; 12];
        assert_eq!(
            decrypt(b"garbage-bytes", &nonce, &key).unwrap_err(),
            DecryptError::DecryptFailed
        );
    }

    #[test]
    fn encrypt_rejects_short_key() {
        let key = [0u8; 16];
        assert_eq!(
            encrypt(b"hi", &key).unwrap_err(),
            EncryptError::InvalidKeyLen(16)
        );
    }

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let key = sample_key();
        let plaintext = b"sk-secret-api-key-for-llm";
        let (ct, nonce) = encrypt(plaintext, &key).unwrap();
        let out = decrypt(&ct, &nonce, &key).unwrap();
        assert_eq!(out.as_slice(), plaintext.as_slice());
    }

    #[test]
    fn encrypt_uses_distinct_nonces() {
        let key = sample_key();
        let (ct1, n1) = encrypt(b"payload", &key).unwrap();
        let (ct2, n2) = encrypt(b"payload", &key).unwrap();
        assert_ne!(n1, n2);
        assert_ne!(ct1, ct2);
        assert_eq!(decrypt(&ct1, &n1, &key).unwrap(), b"payload");
        assert_eq!(decrypt(&ct2, &n2, &key).unwrap(), b"payload");
    }

    #[test]
    fn decrypt_fails_with_wrong_key() {
        let key = sample_key();
        let mut wrong_key = key;
        wrong_key[0] ^= 0xAA;
        let (ct, nonce) = encrypt(b"payload", &key).unwrap();
        assert_eq!(
            decrypt(&ct, &nonce, &wrong_key).unwrap_err(),
            DecryptError::DecryptFailed
        );
    }

    #[test]
    fn decrypt_fails_with_wrong_nonce() {
        let key = sample_key();
        let (ct, nonce) = encrypt(b"payload", &key).unwrap();
        let mut wrong_nonce = nonce.clone();
        wrong_nonce[0] ^= 0x55;
        assert_eq!(
            decrypt(&ct, &wrong_nonce, &key).unwrap_err(),
            DecryptError::DecryptFailed
        );
    }

    #[test]
    fn encrypt_decrypt_empty_plaintext() {
        let key = sample_key();
        let (ct, nonce) = encrypt(b"", &key).unwrap();
        let out = decrypt(&ct, &nonce, &key).unwrap();
        assert!(out.is_empty());
    }
}
