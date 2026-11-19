//! Encryption of the secrets the platform must be able to show again: TAK passwords and the
//! private keys of the certificates it ships in zips.
//!
//! AES-256-GCM under a key derived from the `APP_SECRET` setting. The stored form is
//! `v1:<base64 nonce>:<base64 ciphertext>`.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::engine::general_purpose::STANDARD_NO_PAD as B64;
use base64::Engine;
use sha2::{Digest, Sha256};

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("stored secret is malformed")]
    Malformed,
    #[error("stored secret cannot be decrypted with the current application key")]
    Decrypt,
}

/// Key material derived once from the application secret.
#[derive(Clone)]
pub struct SecretBox {
    cipher: Aes256Gcm,
    mac_key: [u8; 32],
}

fn derive(app_secret: &[u8], purpose: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"tak-manager/");
    h.update(purpose);
    h.update(b"/");
    h.update(app_secret);
    h.finalize().into()
}

impl SecretBox {
    pub fn new(app_secret: &[u8]) -> Self {
        let key = derive(app_secret, b"secret-box");
        Self { cipher: Aes256Gcm::new_from_slice(&key).expect("32-byte key"), mac_key: derive(app_secret, b"mac") }
    }

    /// Key for the HMACs of the application (login codes, password tags).
    pub fn mac_key(&self) -> &[u8] {
        &self.mac_key
    }

    /// Encrypts `plain`. `context` is authenticated with it (a column name, for example), so a
    /// value copied into another column does not decrypt.
    pub fn seal(&self, context: &str, plain: &[u8]) -> String {
        let mut nonce = [0u8; 12];
        rand::fill(&mut nonce);
        let ct = self
            .cipher
            .encrypt(&Nonce::from(nonce), Payload { msg: plain, aad: context.as_bytes() })
            .expect("AES-GCM encryption cannot fail for these sizes");
        format!("v1:{}:{}", B64.encode(nonce), B64.encode(ct))
    }

    pub fn open(&self, context: &str, stored: &str) -> Result<Vec<u8>, SecretError> {
        let mut parts = stored.splitn(3, ':');
        let (Some("v1"), Some(nonce), Some(ct)) = (parts.next(), parts.next(), parts.next()) else {
            return Err(SecretError::Malformed);
        };
        let nonce = B64.decode(nonce).map_err(|_| SecretError::Malformed)?;
        let ct = B64.decode(ct).map_err(|_| SecretError::Malformed)?;
        let nonce: [u8; 12] = nonce.as_slice().try_into().map_err(|_| SecretError::Malformed)?;
        self.cipher.decrypt(&Nonce::from(nonce), Payload { msg: &ct, aad: context.as_bytes() }).map_err(|_| SecretError::Decrypt)
    }

    pub fn open_string(&self, context: &str, stored: &str) -> Result<String, SecretError> {
        String::from_utf8(self.open(context, stored)?).map_err(|_| SecretError::Malformed)
    }

    /// Short fingerprint of a TAK password, stored in the directory next to the account. The
    /// reconciliation loop compares it with the expected one to know whether the password in
    /// the directory is the current one, without reading or testing the password itself.
    pub fn password_tag(&self, username: &str, password: &str) -> String {
        crate::login::hash_code(&self.mac_key, username, password)[..32].to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let b = SecretBox::new(b"an application secret");
        let sealed = b.seal("tak_accesses.password", b"s3cret");
        assert!(sealed.starts_with("v1:"));
        assert_eq!(b.open_string("tak_accesses.password", &sealed).unwrap(), "s3cret");
    }

    #[test]
    fn two_seals_differ() {
        let b = SecretBox::new(b"k");
        assert_ne!(b.seal("c", b"x"), b.seal("c", b"x"));
    }

    #[test]
    fn wrong_key_context_or_tampering_is_refused() {
        let b = SecretBox::new(b"k");
        let sealed = b.seal("c", b"x");
        assert!(matches!(SecretBox::new(b"other").open("c", &sealed), Err(SecretError::Decrypt)));
        assert!(matches!(b.open("d", &sealed), Err(SecretError::Decrypt)));
        let mut tampered = sealed.clone();
        tampered.pop();
        tampered.push(if sealed.ends_with('A') { 'B' } else { 'A' });
        assert!(b.open("c", &tampered).is_err());
        assert!(matches!(b.open("c", "nonsense"), Err(SecretError::Malformed)));
    }

    #[test]
    fn password_tag_changes_with_password_and_user() {
        let b = SecretBox::new(b"k");
        let t = b.password_tag("alice", "pw1");
        assert_eq!(t.len(), 32);
        assert_eq!(t, b.password_tag("alice", "pw1"));
        assert_ne!(t, b.password_tag("alice", "pw2"));
        assert_ne!(t, b.password_tag("bob", "pw1"));
    }
}
