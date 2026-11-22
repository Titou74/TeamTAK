//! What the platform asks of TAK Server: issue and revoke certificates, push group changes
//! to connected clients, list and cut sessions, register video feeds, keep device profiles.
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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Subscription {
    /// Identifier used to cut the session.
    pub subscription_uid: String,
    /// Empty for a session that has no group: TAK Server then lists it without its account.
    pub username: String,
    pub callsign: Option<String>,
    pub client_uid: Option<String>,
    /// Groups the session currently has, which can lag behind the directory.
    pub groups: Vec<String>,
    /// When TAK Server last received something from the client.
    pub last_report_at: Option<OffsetDateTime>,
    /// Address the client connects from, as TAK Server sees it.
    pub ip_address: Option<String>,
    /// The client software and its version, when it announced them (`ATAK-CIV 5.4.0`).
    pub client: Option<String>,
}

/// A video feed of TAK Server's video manager, as the platform reads and writes it: one feed
/// per video collection, both with the same identifier and name. TAK Server hands it to the
/// clients that share a group with it (ATAK lists it among its videos).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoFeed {
    pub uid: String,
    /// The name devices show.
    pub alias: String,
    /// Where devices play it from, credentials included.
    pub url: String,
}

/// When TAK Server hands a device profile to a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProfileKind {
    /// When the device enrolls for its certificate (port 8446).
    Enrollment,
    /// When the device connects and asks for profiles (ATAK's "Apply TAK Server profile
    /// updates").
    Connection,
}

impl ProfileKind {
    /// The `type` of a profile in TAK Server's API.
    pub fn as_tak_type(self) -> &'static str {
        match self {
            ProfileKind::Enrollment => "Enrollment",
            ProfileKind::Connection => "Connection",
        }
    }

    pub fn from_tak_type(raw: &str) -> Option<Self> {
        match raw {
            "Enrollment" => Some(ProfileKind::Enrollment),
            "Connection" => Some(ProfileKind::Connection),
            _ => None,
        }
    }
}

/// A device profile of TAK Server: a set of files handed to the devices of its groups, as a
/// data package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceProfile {
    pub id: i64,
    pub name: String,
    /// `None` for another kind (a profile for a tool).
    pub kind: Option<ProfileKind>,
    pub active: bool,
    pub groups: std::collections::BTreeSet<String>,
}

/// A file of a device profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileFile {
    pub id: i64,
    pub name: String,
    pub data: Vec<u8>,
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
    /// TAK Server 5.8 answers 400 for any identifier it has no connection for: a session it
    /// does not list, sometimes one it has just cut, and a leftover entry it keeps listing
    /// after the connection is gone (see [`crate::reconcile::settle_failed_cuts`]).
    async fn cut_subscription(&self, subscription_uid: &str) -> Result<(), TakError>;
    /// Every certificate issued through enrollment that TAK Server still accepts: neither
    /// revoked nor expired, including those a device replaced by enrolling again.
    async fn active_certificates(&self) -> Result<Vec<IssuedCertificate>, TakError>;
    /// Revokes a certificate; its open connections are closed by the server.
    async fn revoke_certificate(&self, id: i64) -> Result<(), TakError>;
    /// Obtains a certificate for `username` the way a TAK client does: a key and a signing
    /// request made here, signed by the server after it checked `password` in the directory.
    async fn enroll(&self, username: &str, password: &str, client_uid: &str) -> Result<EnrolledCertificate, TakError>;
    /// Every video feed of the video manager. The platform's certificate sees them all,
    /// whatever their group.
    async fn video_feeds(&self) -> Result<Vec<VideoFeed>, TakError>;
    /// Registers a feed for the members of `group` only.
    async fn add_video_feed(&self, feed: &VideoFeed, group: &str) -> Result<(), TakError>;
    /// Changes the name and address of a feed; its group stays.
    async fn update_video_feed(&self, feed: &VideoFeed) -> Result<(), TakError>;
    async fn delete_video_feed(&self, uid: &str) -> Result<(), TakError>;
    /// Every device profile, whatever its groups.
    async fn device_profiles(&self) -> Result<Vec<DeviceProfile>, TakError>;
    /// The files of a profile, with their contents.
    async fn profile_files(&self, profile: &str) -> Result<Vec<ProfileFile>, TakError>;
    /// Creates a profile for `groups`. TAK Server makes it an active enrollment profile.
    async fn create_device_profile(&self, name: &str, groups: &[String]) -> Result<(), TakError>;
    /// Sets the kind and the groups of a profile, and makes it active.
    async fn set_device_profile(&self, id: i64, name: &str, kind: ProfileKind, groups: &[String]) -> Result<(), TakError>;
    async fn add_profile_file(&self, profile: &str, file_name: &str, data: &[u8]) -> Result<(), TakError>;
    async fn delete_profile_file(&self, profile: &str, file_id: i64) -> Result<(), TakError>;
    /// Removes a profile and its files.
    async fn delete_device_profile(&self, id: i64) -> Result<(), TakError>;
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
