//! Client side of TAK Server's certificate enrollment: key, signing request, and reading the
//! signed certificate back.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair};
use rsa::pkcs8::EncodePrivateKey;
use time::OffsetDateTime;
use x509_parser::prelude::{FromDer, X509Certificate};

use super::{EnrolledCertificate, TakError};

/// Key size used by ATAK and by the server's own scripts.
const RSA_BITS: usize = 2048;

pub struct KeyAndCsr {
    /// PKCS#8, unencrypted.
    pub private_key_der: Vec<u8>,
    pub csr_der: Vec<u8>,
}

fn crypto(e: impl std::fmt::Display) -> TakError {
    TakError::Crypto(e.to_string())
}

/// Reads the `<nameEntry name="O" value="TAK"/>` elements of the enrollment configuration
/// (`GET /Marti/api/tls/config`): the subject fields the server requires in a request.
pub fn parse_name_entries(xml: &str) -> Vec<(String, String)> {
    fn attr(tag: &str, name: &str) -> Option<String> {
        let start = tag.find(&format!("{name}=\""))? + name.len() + 2;
        let end = tag[start..].find('"')? + start;
        Some(tag[start..end].to_string())
    }
    let mut entries = Vec::new();
    let mut rest = xml;
    while let Some(pos) = rest.find("nameEntry ") {
        rest = &rest[pos..];
        let end = rest.find('>').unwrap_or(rest.len());
        let tag = &rest[..end];
        if let (Some(name), Some(value)) = (attr(tag, "name"), attr(tag, "value")) {
            entries.push((name, value));
        }
        rest = &rest[end..];
    }
    entries
}

fn dn_type(name: &str) -> Option<DnType> {
    Some(match name.to_ascii_uppercase().as_str() {
        "O" => DnType::OrganizationName,
        "OU" => DnType::OrganizationalUnitName,
        "C" => DnType::CountryName,
        "ST" => DnType::StateOrProvinceName,
        "L" => DnType::LocalityName,
        _ => return None,
    })
}

/// Generates an RSA key and a signing request whose subject is the server's name entries
/// followed by `CN=<common_name>`. CPU-bound (about 100 ms): call it from a blocking task.
pub fn generate_key_and_csr(name_entries: &[(String, String)], common_name: &str) -> Result<KeyAndCsr, TakError> {
    let key = rsa::PrivateKey::new(&mut rand::rng(), RSA_BITS).map_err(crypto)?;
    let private_key_der = key.to_pkcs8_der().map_err(crypto)?.as_bytes().to_vec();
    let key_pair = KeyPair::try_from(private_key_der.as_slice()).map_err(crypto)?;

    let mut dn = DistinguishedName::new();
    for (name, value) in name_entries {
        let ty = dn_type(name).ok_or_else(|| TakError::Protocol(format!("unsupported name entry {name}")))?;
        dn.push(ty, value.as_str());
    }
    dn.push(DnType::CommonName, common_name);
    let mut params = CertificateParams::new(Vec::<String>::new()).map_err(crypto)?;
    params.distinguished_name = dn;
    let csr = params.serialize_request(&key_pair).map_err(crypto)?;
    Ok(KeyAndCsr { private_key_der, csr_der: csr.der().to_vec() })
}

/// Body of `POST /Marti/api/tls/signClient/v2`: the request in base64, without PEM armour.
pub fn csr_body(csr_der: &[u8]) -> String {
    B64.encode(csr_der)
}

/// Decodes a certificate as the server returns it: base64 DER, with or without PEM armour
/// and line breaks.
pub fn decode_certificate(text: &str) -> Result<Vec<u8>, TakError> {
    let b64: String =
        text.lines().filter(|l| !l.starts_with("-----")).flat_map(|l| l.chars()).filter(|c| !c.is_whitespace()).collect();
    B64.decode(b64).map_err(|e| TakError::Protocol(format!("certificate is not base64: {e}")))
}

/// Validity and subject CN of a DER certificate.
pub fn inspect_certificate(der: &[u8]) -> Result<(String, OffsetDateTime, OffsetDateTime), TakError> {
    let (_, cert) = X509Certificate::from_der(der).map_err(|e| TakError::Protocol(format!("not a certificate: {e}")))?;
    let cn = cert.subject().iter_common_name().next().and_then(|cn| cn.as_str().ok()).unwrap_or_default().to_string();
    let ts = |t: x509_parser::time::ASN1Time| OffsetDateTime::from_unix_timestamp(t.timestamp()).map_err(crypto);
    Ok((cn, ts(cert.validity().not_before)?, ts(cert.validity().not_after)?))
}

/// Assembles the result of an enrollment from the JSON answer of `signClient/v2`
/// (`signedCert`, `ca0`, `ca1`, …) and checks that the certificate is for `username`.
pub fn read_signed_response(
    username: &str,
    private_key_der: Vec<u8>,
    response: &serde_json::Value,
) -> Result<EnrolledCertificate, TakError> {
    let obj = response.as_object().ok_or_else(|| TakError::Protocol("signClient answer is not an object".into()))?;
    let signed = obj
        .get("signedCert")
        .and_then(|v| v.as_str())
        .ok_or_else(|| TakError::Protocol("signClient answer has no signedCert".into()))?;
    let certificate_der = decode_certificate(signed)?;
    let mut cas: Vec<(&String, Vec<u8>)> = Vec::new();
    for (key, value) in obj {
        if key.starts_with("ca") {
            if let Some(text) = value.as_str() {
                cas.push((key, decode_certificate(text)?));
            }
        }
    }
    cas.sort_by(|a, b| a.0.cmp(b.0));
    let (cn, not_before, not_after) = inspect_certificate(&certificate_der)?;
    if cn != username {
        return Err(TakError::Protocol(format!("certificate issued for {cn:?} instead of {username:?}")));
    }
    Ok(EnrolledCertificate {
        fingerprint: super::fingerprint(&certificate_der),
        certificate_der,
        private_key_der,
        ca_ders: cas.into_iter().map(|(_, der)| der).collect(),
        not_before,
        not_after,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use x509_parser::certification_request::X509CertificationRequest;

    #[test]
    fn name_entries_are_read_from_the_enrollment_configuration() {
        let xml = r#"<?xml version="1.0"?><ns2:certificateConfig xmlns="http://bbn.com/marti/xml/config">
            <nameEntries><nameEntry name="O" value="TAK"/><nameEntry value="TAKManager" name="OU"/></nameEntries>
            </ns2:certificateConfig>"#;
        assert_eq!(
            parse_name_entries(xml),
            vec![("O".to_string(), "TAK".to_string()), ("OU".to_string(), "TAKManager".to_string())]
        );
        assert!(parse_name_entries("<certificateConfig/>").is_empty());
    }

    #[test]
    fn csr_carries_the_expected_subject() {
        let entries = vec![("O".to_string(), "TAK".to_string()), ("OU".to_string(), "TAKManager".to_string())];
        let made = generate_key_and_csr(&entries, "alpha-shared").unwrap();
        let (_, csr) = X509CertificationRequest::from_der(&made.csr_der).unwrap();
        assert_eq!(csr.certification_request_info.subject.to_string(), "O=TAK, OU=TAKManager, CN=alpha-shared");
        assert!(!csr_body(&made.csr_der).contains('\n'));
        assert!(!csr_body(&made.csr_der).contains("BEGIN"));
    }

    #[test]
    fn unknown_name_entries_are_refused() {
        assert!(generate_key_and_csr(&[("DC".to_string(), "x".to_string())], "a").is_err());
    }

    #[test]
    fn certificates_decode_with_or_without_armour() {
        let der = vec![1u8, 2, 3, 4, 5, 6, 7];
        let b64 = B64.encode(&der);
        assert_eq!(decode_certificate(&b64).unwrap(), der);
        let pem = format!("-----BEGIN CERTIFICATE-----\n{}\n{}\n-----END CERTIFICATE-----\n", &b64[..4], &b64[4..]);
        assert_eq!(decode_certificate(&pem).unwrap(), der);
        assert!(decode_certificate("***").is_err());
    }
}
