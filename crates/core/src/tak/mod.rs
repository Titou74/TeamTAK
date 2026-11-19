//! What the platform asks of TAK Server: issue and revoke certificates, push group changes
//! to connected clients, list and cut sessions.
//!
//! [`TakServer`] is the only way the rest of the code reaches it. The production backend
//! talks to the TAK Server 5.8 HTTPS API ([`http::HttpTakServer`]);
//! [`memory::MemoryTakServer`] backs the unit tests.

pub mod enroll;
pub mod http;
pub mod memory;

use async_trait::async_trait;
use time::OffsetDateTime;

#[derive(Debug, thiserror::Error)]
pub enum TakError {
    #[error("TAK Server unreachable: {0}")]
    Unavailable(String),
    #[error("TAK Server answered {status} to {call}")]
    Status { call: String, status: u16 },
    #[error("unexpected answer from TAK Server: {0}")]
    Protocol(String),
    #[error("certificate handling failed: {0}")]
    Crypto(String),
}

/// A client certificate issued by TAK Server's enrollment and not revoked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuedCertificate {
    /// Identifier used by the revocation call.
    pub id: i64,
    /// The account it was issued to (CN of the certificate).
    pub username: String,
    /// SHA-256 of the certificate, lowercase hexadecimal without separators.
    pub fingerprint: String,
    pub client_uid: Option<String>,
    pub issued_at: Option<OffsetDateTime>,
    pub expires_at: Option<OffsetDateTime>,
}

/// A client connected to TAK Server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subscription {
    /// Identifier used to cut the session.
    pub subscription_uid: String,
    pub username: String,
    pub callsign: Option<String>,
    pub client_uid: Option<String>,
    /// Groups the session currently has, which can lag behind the directory.
    pub groups: Vec<String>,
}

/// A certificate the platform obtained on behalf of an account.
#[derive(Clone)]
pub struct EnrolledCertificate {
    pub certificate_der: Vec<u8>,
    /// PKCS#8, unencrypted.
    pub private_key_der: Vec<u8>,
    /// Issuing chain returned by the server, DER.
    pub ca_ders: Vec<Vec<u8>>,
    /// SHA-256 of the certificate, lowercase hexadecimal without separators.
    pub fingerprint: String,
    pub not_before: OffsetDateTime,
    pub not_after: OffsetDateTime,
}

impl std::fmt::Debug for EnrolledCertificate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnrolledCertificate")
            .field("fingerprint", &self.fingerprint)
            .field("not_after", &self.not_after)
            .finish_non_exhaustive()
    }
}

#[async_trait]
pub trait TakServer: Send + Sync {
    /// Version string of the server; also the reachability check.
    async fn version(&self) -> Result<String, TakError>;
    /// Makes the server re-read the groups of `username` now and push them to its open
    /// sessions, instead of waiting for the next periodic refresh.
    async fn refresh_groups(&self, username: &str) -> Result<(), TakError>;
    async fn subscriptions(&self) -> Result<Vec<Subscription>, TakError>;
    async fn cut_subscription(&self, subscription_uid: &str) -> Result<(), TakError>;
    /// Every certificate issued through enrollment that TAK Server still accepts: neither
    /// revoked nor expired, including those a device replaced by enrolling again.
    async fn active_certificates(&self) -> Result<Vec<IssuedCertificate>, TakError>;
    /// Revokes a certificate; its open connections are closed by the server.
    async fn revoke_certificate(&self, id: i64) -> Result<(), TakError>;
    /// Obtains a certificate for `username` the way a TAK client does: a key and a signing
    /// request made here, signed by the server after it checked `password` in the directory.
    async fn enroll(&self, username: &str, password: &str, client_uid: &str) -> Result<EnrolledCertificate, TakError>;
}

/// SHA-256 fingerprint in the form used across the platform.
pub fn fingerprint(der: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(der))
}

/// Brings a fingerprint as printed by TAK Server or OpenSSL (`AB:CD:…`) to the platform form.
pub fn normalize_fingerprint(raw: &str) -> String {
    raw.chars().filter(|c| c.is_ascii_hexdigit()).collect::<String>().to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints_are_comparable() {
        assert_eq!(normalize_fingerprint("AB:cd:01"), "abcd01");
        assert_eq!(fingerprint(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
