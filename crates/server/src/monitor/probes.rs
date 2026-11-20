//! What the platform checks by itself, over the Docker network: do TAK Server's ports
//! answer, which certificate does each present, does the directory listen. Every probe has
//! a short timeout and none runs while a user's request waits.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use teamtak_core::certs::{parse_der, CertificateInfo};

pub const PROBE_TIMEOUT: Duration = Duration::from_secs(4);

/// Accepts any certificate and keeps the one the server presented. The probe reads an
/// expiry date; it sends nothing and trusts nothing it receives.
#[derive(Debug)]
struct Capture {
    seen: Mutex<Option<Vec<u8>>>,
    schemes: Vec<SignatureScheme>,
}

impl ServerCertVerifier for Capture {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        *self.seen.lock().unwrap() = Some(end_entity.to_vec());
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(&self, _: &[u8], _: &CertificateDer<'_>, _: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(&self, _: &[u8], _: &CertificateDer<'_>, _: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.schemes.clone()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TlsProbe {
    /// The port accepted a connection.
    pub open: bool,
    /// The certificate presented, when the TLS exchange got that far.
    pub certificate: Option<CertificateInfo>,
}

/// True when `host:port` accepts a TCP connection in time.
pub async fn tcp_open(host: &str, port: u16) -> bool {
    matches!(tokio::time::timeout(PROBE_TIMEOUT, tokio::net::TcpStream::connect((host, port))).await, Ok(Ok(_)))
}

/// Connects to `host:port`, starts TLS announcing `server_name`, and reports the certificate
/// the server presents. A server that then refuses the client (TAK Server asks for a client
/// certificate on two of its ports) has still shown its own.
pub async fn tls_probe(host: &str, port: u16, server_name: &str) -> TlsProbe {
    let Ok(Ok(tcp)) = tokio::time::timeout(PROBE_TIMEOUT, tokio::net::TcpStream::connect((host, port))).await else {
        return TlsProbe { open: false, certificate: None };
    };
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let capture = Arc::new(Capture { seen: Mutex::new(None), schemes: provider.signature_verification_algorithms.supported_schemes() });
    let config = rustls::ClientConfig::builder_with_provider(provider).with_safe_default_protocol_versions().map(|builder| {
        builder.dangerous().with_custom_certificate_verifier(capture.clone()).with_no_client_auth()
    });
    let (Ok(config), Ok(name)) = (config, ServerName::try_from(server_name.to_string())) else {
        return TlsProbe { open: true, certificate: None };
    };
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    // The outcome of the handshake does not matter: the certificate was captured on the way.
    let _ = tokio::time::timeout(PROBE_TIMEOUT, connector.connect(name, tcp)).await;
    let der = capture.seen.lock().unwrap().take();
    TlsProbe { open: true, certificate: der.and_then(|der| parse_der(&der).ok()) }
}

/// Host and port of an address such as `https://takserver:8443` or `ldap://openldap:389`.
pub fn host_port(url: &str, default_port: u16) -> Option<(String, u16)> {
    let rest = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit_once('@').map(|(_, a)| a).unwrap_or(authority);
    match authority.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() => Some((host.to_string(), port.parse().ok()?)),
        None if !authority.is_empty() => Some((authority.to_string(), default_port)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_and_ports_of_the_configured_addresses() {
        assert_eq!(host_port("https://takserver:8443", 443), Some(("takserver".into(), 8443)));
        assert_eq!(host_port("https://tak.example.org:8446/", 443), Some(("tak.example.org".into(), 8446)));
        assert_eq!(host_port("ldap://openldap:389", 389), Some(("openldap".into(), 389)));
        assert_eq!(host_port("ldap://openldap", 389), Some(("openldap".into(), 389)));
        assert_eq!(host_port("caddy:443", 443), Some(("caddy".into(), 443)));
        assert_eq!(host_port("https://tak.example.org", 443), Some(("tak.example.org".into(), 443)));
        assert_eq!(host_port("https://:8443", 443), None);
        assert_eq!(host_port("https://host:notaport", 443), None);
    }

    #[tokio::test]
    async fn a_closed_port_is_reported_closed() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(tcp_open("127.0.0.1", port).await);
        drop(listener);
        assert!(!tcp_open("127.0.0.1", port).await);
        assert_eq!(tls_probe("127.0.0.1", port, "localhost").await, TlsProbe { open: false, certificate: None });
    }

    #[tokio::test]
    async fn an_open_port_that_does_not_speak_tls_is_open_without_certificate() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            // Accepts and closes: not a TLS server.
            while let Ok((stream, _)) = listener.accept().await {
                drop(stream);
            }
        });
        assert_eq!(tls_probe("127.0.0.1", port, "localhost").await, TlsProbe { open: true, certificate: None });
    }
}
