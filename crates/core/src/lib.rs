//! Domain rules and TAK/LDAP adapters of TeamTAK.
//!
//! Nothing here knows about HTTP or PostgreSQL. The server crate loads the desired state from
//! its database, asks [`reconcile`] what has to change, and applies the result through the
//! [`directory::Directory`] and [`tak::TakServer`] traits, so either backend can be replaced.

pub mod alerts;
pub mod certs;
pub mod challenge;
pub mod directory;
pub mod events;
pub mod login;
pub mod names;
pub mod package;
pub mod policy;
pub mod reconcile;
pub mod secret;
pub mod tak;
pub mod versions;
pub mod video;

/// Random string drawn from `alphabet` with a uniform distribution.
pub fn random_string(len: usize, alphabet: &[u8]) -> String {
    use rand::RngExt;
    let mut rng = rand::rng();
    (0..len).map(|_| alphabet[rng.random_range(0..alphabet.len())] as char).collect()
}

/// Letters and digits that cannot be mistaken for one another when read aloud or typed
/// on a phone (no 0/O, 1/l/I).
pub const UNAMBIGUOUS: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";

/// Password of a TAK account: 24 characters, about 140 bits. Long enough that no lockout is
/// needed on the enrollment port, and free of characters that need escaping in a URL or XML.
pub fn generate_tak_password() -> String {
    random_string(24, UNAMBIGUOUS)
}

/// An HTTPS client for public servers (container registries, a webhook), trusting the
/// usual certificate authorities. Plain `http://` addresses work too.
pub fn public_http_client(timeout: std::time::Duration) -> Result<reqwest::Client, String> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let tls = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_root_certificates(roots)
        .with_no_client_auth();
    reqwest::Client::builder()
        .tls_backend_preconfigured(tls)
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(timeout)
        .user_agent(concat!("teamtak/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())
}
