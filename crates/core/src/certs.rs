//! Reading certificates for the monitoring: who they are for and when they end.

use time::OffsetDateTime;
use x509_parser::prelude::{FromDer, X509Certificate};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertificateInfo {
    /// Common name of the subject; empty when it has none.
    pub subject: String,
    pub issuer: String,
    pub not_before: OffsetDateTime,
    pub not_after: OffsetDateTime,
    pub is_ca: bool,
    /// SHA-256 of the certificate, lowercase hexadecimal.
    pub fingerprint: String,
}

impl CertificateInfo {
    pub fn self_signed(&self) -> bool {
        self.subject == self.issuer
    }
}

fn common_name(name: &x509_parser::x509::X509Name<'_>) -> String {
    name.iter_common_name().next().and_then(|cn| cn.as_str().ok()).unwrap_or_default().to_string()
}

/// Reads one DER certificate.
pub fn parse_der(der: &[u8]) -> Result<CertificateInfo, String> {
    let (_, cert) = X509Certificate::from_der(der).map_err(|e| format!("not a certificate: {e}"))?;
    Ok(CertificateInfo {
        subject: common_name(cert.subject()),
        issuer: common_name(cert.issuer()),
        not_before: cert.validity().not_before.to_datetime(),
        not_after: cert.validity().not_after.to_datetime(),
        is_ca: cert.is_ca(),
        fingerprint: crate::tak::fingerprint(der),
    })
}

/// Reads every certificate of a PEM file, in order.
pub fn parse_pem(pem: &[u8]) -> Result<Vec<CertificateInfo>, String> {
    use rustls_pki_types::pem::PemObject;
    use rustls_pki_types::CertificateDer;
    let mut found = Vec::new();
    for der in CertificateDer::pem_slice_iter(pem) {
        found.push(parse_der(&der.map_err(|e| format!("invalid PEM: {e}"))?)?);
    }
    Ok(found)
}

/// Whole days before `not_after`; negative once it is past.
pub fn days_left(not_after: OffsetDateTime, now: OffsetDateTime) -> i64 {
    (not_after - now).whole_seconds().div_euclid(86_400)
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn make(cn: &str, ca: bool, not_after: OffsetDateTime) -> Vec<u8> {
        let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        params.distinguished_name = rcgen::DistinguishedName::new();
        params.distinguished_name.push(rcgen::DnType::CommonName, cn);
        params.not_before = datetime!(2026-01-01 0:00 UTC);
        params.not_after = not_after;
        if ca {
            params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        }
        let key = rcgen::KeyPair::generate().unwrap();
        params.self_signed(&key).unwrap().der().to_vec()
    }

    #[test]
    fn subject_expiry_and_role_are_read() {
        let der = make("tak-manager-ca", true, datetime!(2036-11-16 17:13:05 UTC));
        let info = parse_der(&der).unwrap();
        assert_eq!((info.subject.as_str(), info.is_ca, info.self_signed()), ("tak-manager-ca", true, true));
        assert_eq!(info.not_after, datetime!(2036-11-16 17:13:05 UTC));
        assert_eq!(info.fingerprint.len(), 64);
        let leaf = parse_der(&make("takserver", false, datetime!(2028-11-18 0:00 UTC))).unwrap();
        assert!(!leaf.is_ca);
        assert!(parse_der(b"not a certificate").is_err());
    }

    #[test]
    fn a_pem_file_gives_its_certificates_in_order() {
        let pem = |der: &[u8]| {
            use base64::Engine;
            let b64 = base64::engine::general_purpose::STANDARD.encode(der);
            let lines: Vec<&str> = b64.as_bytes().chunks(64).map(|c| std::str::from_utf8(c).unwrap()).collect();
            format!("-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n", lines.join("\n"))
        };
        // The shape of the CA file the platform is given: the intermediate, then the root.
        let chain = format!(
            "Bag Attributes\n    friendlyName: intermediate\n{}{}",
            pem(&make("tak-manager-ca", true, datetime!(2036-11-16 0:00 UTC))),
            pem(&make("tak-manager-root-ca", true, datetime!(2036-11-17 0:00 UTC)))
        );
        let found = parse_pem(chain.as_bytes()).unwrap();
        let names: Vec<&str> = found.iter().map(|c| c.subject.as_str()).collect();
        assert_eq!(names, vec!["tak-manager-ca", "tak-manager-root-ca"]);
        assert_eq!(parse_pem(b"no certificate here").unwrap(), vec![]);
    }

    #[test]
    fn days_left_are_whole_days() {
        let now = datetime!(2026-11-19 12:00 UTC);
        assert_eq!(days_left(datetime!(2026-12-19 12:00 UTC), now), 30);
        assert_eq!(days_left(datetime!(2026-12-19 11:59 UTC), now), 29, "not yet 30 full days");
        assert_eq!(days_left(datetime!(2026-11-19 18:00 UTC), now), 0, "expires today");
        assert_eq!(days_left(datetime!(2026-11-19 11:00 UTC), now), -1, "expired an hour ago");
    }
}
