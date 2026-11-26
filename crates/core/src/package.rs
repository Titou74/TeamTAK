//! What a user is handed to connect a TAK client: connection zips (data packages), PKCS#12
//! stores, and the strings encoded in QR codes.
//!
//! A zip holds the server address, the truststore and, optionally, a client certificate.
//! It never holds a callsign, a team colour, a role or a group: groups come from the
//! directory through TAK Server. It turns on ATAK's channels for the server, so that the
//! device lists those groups, and ATAK's "Apply TAK Server profile updates", so that the
//! device gets its team's maps (`crate::maps`).
//!
//! A zip must work in ATAK, WinTAK, TAK Aware and iTAK: docs/architecture.md ("Connection zips
//! and the clients they are for") says what each reads and what was checked.

use std::io::{Cursor, Write};

use p12_keystore::{Certificate, EncryptionAlgorithm, KeyStore, KeyStoreEntry, MacAlgorithm, PrivateKey, PrivateKeyChain};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum PackageError {
    #[error("PKCS#12 store: {0}")]
    Pkcs12(String),
    #[error("zip archive: {0}")]
    Zip(String),
}

/// Password of the truststore inside a zip. It protects nothing: the truststore only holds
/// public CA certificates, and the password is written next to it in `config.pref`.
/// `atakatak` is the convention of TAK Server's own scripts.
pub const TRUSTSTORE_PASSWORD: &str = "atakatak";
/// File name of the truststore inside a zip.
pub const TRUSTSTORE_FILE: &str = "truststore.p12";

/// ATAK preference that shows its Channels tool.
pub const CHANNELS_PREF: &str = "prefs_enable_channels";

/// ATAK preference that lists a server's groups in that tool, for one server host.
pub fn channels_host_pref(host: &str) -> String {
    format!("prefs_enable_channels_host-{host}")
}

/// Encryption of the PKCS#12 stores.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum P12Format {
    /// AES-256 and HMAC-SHA-256: what TAK Server's `makeCert.sh` produces by default.
    #[default]
    Modern,
    /// Triple DES and HMAC-SHA-1: what `makeCert.sh -legacy` produces, for Android 12 and older.
    Legacy,
}

impl P12Format {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "modern" => Some(P12Format::Modern),
            "legacy" => Some(P12Format::Legacy),
            _ => None,
        }
    }
}

fn p12(e: impl std::fmt::Display) -> PackageError {
    PackageError::Pkcs12(e.to_string())
}

fn write_store(store: &KeyStore, password: &str, format: P12Format) -> Result<Vec<u8>, PackageError> {
    let writer = store.writer(password);
    let writer = match format {
        P12Format::Modern => {
            writer.encryption_algorithm(EncryptionAlgorithm::PbeWithHmacSha256AndAes256).mac_algorithm(MacAlgorithm::HmacSha256)
        }
        P12Format::Legacy => {
            writer.encryption_algorithm(EncryptionAlgorithm::PbeWithShaAnd3KeyTripleDesCbc).mac_algorithm(MacAlgorithm::HmacSha1)
        }
    };
    writer.write().map_err(p12)
}

/// Truststore handed to clients: the CA certificates (root and intermediate) that sign TAK
/// Server's certificate, with no key.
pub fn build_truststore(ca_ders: &[Vec<u8>], format: P12Format) -> Result<Vec<u8>, PackageError> {
    if ca_ders.is_empty() {
        return Err(PackageError::Pkcs12("no CA certificate to put in the truststore".into()));
    }
    let mut store = KeyStore::new();
    for (i, der) in ca_ders.iter().enumerate() {
        store.add_entry(&format!("ca{i}"), KeyStoreEntry::Certificate(Certificate::from_der(der).map_err(p12)?));
    }
    write_store(&store, TRUSTSTORE_PASSWORD, format)
}

/// Client store: private key, its certificate and the issuing chain.
pub fn build_client_store(
    alias: &str,
    private_key_pkcs8_der: &[u8],
    certificate_der: &[u8],
    ca_ders: &[Vec<u8>],
    password: &str,
    format: P12Format,
) -> Result<Vec<u8>, PackageError> {
    let mut chain = vec![Certificate::from_der(certificate_der).map_err(p12)?];
    for der in ca_ders {
        chain.push(Certificate::from_der(der).map_err(p12)?);
    }
    let key = PrivateKey::from_der(private_key_pkcs8_der).map_err(p12)?;
    let mut store = KeyStore::new();
    store.add_entry(alias, KeyStoreEntry::PrivateKeyChain(PrivateKeyChain::new(alias, key, chain)));
    write_store(&store, password, format)
}

/// A client certificate to ship inside a zip.
#[derive(Debug, Clone)]
pub struct ClientStore {
    /// File name inside the zip, for example `alpha-shared.p12`.
    pub file_name: String,
    pub p12: Vec<u8>,
    pub password: String,
}

/// Content of a connection zip.
#[derive(Debug, Clone)]
pub struct ConnectionPackage {
    /// Name of the connection as shown in the client's server list.
    pub description: String,
    /// Public host name or address of TAK Server.
    pub host: String,
    /// Port TAK clients connect to (8089, or another of the platform's client ports).
    pub port: u16,
    pub truststore_p12: Vec<u8>,
    /// With a client store the zip connects as it is; without, the client enrolls with the
    /// username and password the user types.
    pub client: Option<ClientStore>,
    /// File name of the zip, also written in its manifest.
    pub file_name: String,
}

/// Layout of the archive. Only the layout of TAK Server's own `make-connection-package.sh`
/// exists for now; an iTAK-specific variant would be added here if phone tests ask for one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PackageLayout {
    #[default]
    Official,
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

fn entry(key: &str, class: &str, value: &str) -> String {
    format!("    <entry key=\"{}\" class=\"class java.lang.{class}\">{}</entry>\n", xml_escape(key), xml_escape(value))
}

/// `config.pref` of a package, with the same keys as the official script: a single
/// `cot_streams` entry and nothing about the user or the device; ATAK's channels turned on
/// for the server; and the client store named a second time, the way TAK Aware reads it.
pub fn config_pref(package: &ConnectionPackage) -> String {
    let mut s = String::from("<?xml version='1.0' encoding='ASCII' standalone='yes'?>\n<preferences>\n");
    s.push_str("  <preference version=\"1\" name=\"cot_streams\">\n");
    s.push_str(&entry("count", "Integer", "1"));
    s.push_str(&entry("description0", "String", &package.description));
    s.push_str(&entry("enabled0", "Boolean", "true"));
    s.push_str(&entry("connectString0", "String", &format!("{}:{}:ssl", package.host, package.port)));
    s.push_str(&entry("caLocation0", "String", &format!("cert/{TRUSTSTORE_FILE}")));
    s.push_str(&entry("caPassword0", "String", TRUSTSTORE_PASSWORD));
    match &package.client {
        Some(client) => {
            let location = format!("cert/{}", client.file_name);
            s.push_str(&entry("certificateLocation0", "String", &location));
            s.push_str(&entry("clientPassword0", "String", &client.password));
            // TAK Aware (iPhone) finds the client store under these two names only, without
            // the connection's number, wherever they are in the file: with the numbered keys
            // alone it imports the server and the truststore, has no certificate, and its
            // connection fails. They are written here, among the connection's entries, where
            // ATAK reads numbered keys only and ignores them: under
            // `com.atakmap.app_preferences` ATAK would make this certificate the device's
            // default one, for its other servers too.
            s.push_str(&entry("certificateLocation", "String", &location));
            s.push_str(&entry("clientPassword", "String", &client.password));
        }
        None => {
            s.push_str(&entry("enrollForCertificateWithTrust0", "Boolean", "true"));
            s.push_str(&entry("useAuth0", "Boolean", "true"));
            s.push_str(&entry("cacheCreds0", "String", "Cache credentials"));
        }
    }
    s.push_str("  </preference>\n");
    s.push_str("  <preference version=\"1\" name=\"com.atakmap.app_preferences\">\n");
    s.push_str(&entry("displayServerConnectionWidget", "Boolean", "true"));
    // ATAK shows its Channels tool, with this server's groups in it, only once these two
    // preferences are set. TAK Server hands them out in its device profiles when channels are
    // on (`x509useGroupCache`), but a device that connects with the zip's certificate never
    // enrolls, and ATAK asks for profiles on connection only when the user ticked it: such a
    // device would never show its team's channel. Same keys and type (strings) as TAK
    // Server's own profile. The list itself comes from TAK Server's API on 8443.
    s.push_str(&entry(CHANNELS_PREF, "String", "true"));
    s.push_str(&entry(&channels_host_pref(&package.host), "String", "true"));
    // Off by default in ATAK: without it the device never asks TAK Server for the profiles
    // that carry its team's maps (a device that enrolls gets it from the enrollment profile).
    s.push_str(&entry(crate::maps::PROFILES_ON_CONNECT_PREF, "Boolean", "true"));
    s.push_str("  </preference>\n</preferences>\n");
    s
}

fn manifest(package: &ConnectionPackage, uid: &Uuid) -> String {
    let mut s = String::from("<MissionPackageManifest version=\"2\">\n  <Configuration>\n");
    s.push_str(&format!("    <Parameter name=\"uid\" value=\"{uid}\"/>\n"));
    s.push_str(&format!("    <Parameter name=\"name\" value=\"{}\"/>\n", xml_escape(&package.file_name)));
    s.push_str("    <Parameter name=\"onReceiveDelete\" value=\"true\"/>\n  </Configuration>\n  <Contents>\n");
    let mut files = vec!["config.pref".to_string(), TRUSTSTORE_FILE.to_string()];
    if let Some(client) = &package.client {
        files.push(client.file_name.clone());
    }
    for file in files {
        s.push_str(&format!("    <Content ignore=\"false\" zipEntry=\"{}\"/>\n", xml_escape(&file)));
    }
    s.push_str("  </Contents>\n</MissionPackageManifest>\n");
    s
}

/// Builds a connection zip. This is the only place that knows the layout of the archive.
pub fn build_connection_zip(package: &ConnectionPackage, layout: PackageLayout) -> Result<Vec<u8>, PackageError> {
    let zip_err = |e: zip::result::ZipError| PackageError::Zip(e.to_string());
    let io_err = |e: std::io::Error| PackageError::Zip(e.to_string());
    match layout {
        PackageLayout::Official => {
            // Files at the root and MANIFEST/manifest.xml, as make-connection-package.sh does;
            // the client copies the stores to its cert/ directory, hence the cert/ paths in
            // config.pref.
            let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
            let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
            zip.start_file("config.pref", options).map_err(zip_err)?;
            zip.write_all(config_pref(package).as_bytes()).map_err(io_err)?;
            zip.start_file("MANIFEST/manifest.xml", options).map_err(zip_err)?;
            zip.write_all(manifest(package, &Uuid::new_v4()).as_bytes()).map_err(io_err)?;
            zip.start_file(TRUSTSTORE_FILE, options).map_err(zip_err)?;
            zip.write_all(&package.truststore_p12).map_err(io_err)?;
            if let Some(client) = &package.client {
                zip.start_file(client.file_name.as_str(), options).map_err(zip_err)?;
                zip.write_all(&client.p12).map_err(io_err)?;
            }
            Ok(zip.finish().map_err(zip_err)?.into_inner())
        }
    }
}

/// String of the iTAK "quick connect" QR code: description, host, port, protocol.
/// The description cannot contain a comma, the field separator.
pub fn itak_qr(description: &str, host: &str, port: u16) -> String {
    format!("{},{host},{port},ssl", description.replace(',', " "))
}

fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// URI of the ATAK enrollment QR code. The password travels in the QR code (`token`).
pub fn atak_enroll_uri(host: &str, username: &str, password: &str) -> String {
    format!(
        "tak://com.atakmap.app/enroll?host={}&username={}&token={}",
        percent_encode(host),
        percent_encode(username),
        percent_encode(password)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use p12_keystore::Pkcs12ImportPolicy;
    use rcgen::{CertificateParams, DnType, KeyPair};
    use std::io::Read;

    /// A throwaway CA and a client certificate signed by it.
    fn test_chain() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let ca_key = KeyPair::generate().unwrap();
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        ca_params.distinguished_name.push(DnType::CommonName, "test-ca");
        let ca_cert = ca_params.self_signed(&ca_key).unwrap();
        let issuer = rcgen::Issuer::new(ca_params, ca_key);

        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
        params.distinguished_name.push(DnType::CommonName, "alpha-shared");
        let cert = params.signed_by(&key, &issuer).unwrap();
        (ca_cert.der().to_vec(), cert.der().to_vec(), key.serialize_der())
    }

    fn package(client: Option<ClientStore>) -> ConnectionPackage {
        let (ca, _, _) = test_chain();
        ConnectionPackage {
            description: "TAK <Manager> & co".into(),
            host: "tak.example.org".into(),
            port: 8089,
            truststore_p12: build_truststore(&[ca], P12Format::Modern).unwrap(),
            client,
            file_name: "alpha.zip".into(),
        }
    }

    fn unzip(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        (0..archive.len())
            .map(|i| {
                let mut f = archive.by_index(i).unwrap();
                let mut data = Vec::new();
                f.read_to_end(&mut data).unwrap();
                (f.name().unwrap().to_string(), data)
            })
            .collect()
    }

    /// Keys a zip may set, all about the connection itself.
    const ALLOWED_KEYS: &[&str] = &[
        "count",
        "description0",
        "enabled0",
        "connectString0",
        "caLocation0",
        "caPassword0",
        "certificateLocation0",
        "clientPassword0",
        "certificateLocation",
        "clientPassword",
        "enrollForCertificateWithTrust0",
        "useAuth0",
        "cacheCreds0",
        "displayServerConnectionWidget",
        "prefs_enable_channels",
        "prefs_enable_channels_host-tak.example.org",
        "deviceProfileEnableOnConnect",
    ];

    fn keys(pref: &str) -> Vec<String> {
        pref.split("key=\"").skip(1).map(|s| s[..s.find('"').unwrap()].to_string()).collect()
    }

    #[test]
    fn enrollment_zip_has_three_files_and_asks_for_credentials() {
        let files = unzip(&build_connection_zip(&package(None), PackageLayout::Official).unwrap());
        let names: Vec<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["config.pref", "MANIFEST/manifest.xml", "truststore.p12"]);
        let pref = String::from_utf8(files[0].1.clone()).unwrap();
        assert!(pref.contains(r#"<entry key="connectString0" class="class java.lang.String">tak.example.org:8089:ssl</entry>"#));
        assert!(pref.contains(r#"<entry key="enrollForCertificateWithTrust0" class="class java.lang.Boolean">true</entry>"#));
        assert!(pref.contains("cert/truststore.p12"));
        assert!(!pref.contains("certificateLocation0"));
        assert!(pref.contains("TAK &lt;Manager&gt; &amp; co"), "description is escaped");
        let manifest = String::from_utf8(files[1].1.clone()).unwrap();
        assert!(manifest.contains(r#"zipEntry="config.pref""#) && manifest.contains(r#"zipEntry="truststore.p12""#));
        assert!(manifest.contains("onReceiveDelete"));
    }

    #[test]
    fn certificate_zip_has_four_files_and_no_enrollment() {
        let (ca, cert, key) = test_chain();
        let p12 = build_client_store("alpha-shared", &key, &cert, &[ca], "s3cret-pass", P12Format::Modern).unwrap();
        let client = ClientStore { file_name: "alpha-shared.p12".into(), p12: p12.clone(), password: "s3cret-pass".into() };
        let files = unzip(&build_connection_zip(&package(Some(client)), PackageLayout::Official).unwrap());
        let names: Vec<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["config.pref", "MANIFEST/manifest.xml", "truststore.p12", "alpha-shared.p12"]);
        let pref = String::from_utf8(files[0].1.clone()).unwrap();
        assert!(pref.contains("cert/alpha-shared.p12") && pref.contains("s3cret-pass"));
        assert!(!pref.contains("enrollForCertificateWithTrust0") && !pref.contains("useAuth0"));
        assert_eq!(files[3].1, p12);
    }

    /// The value of a preference and the name of the `<preference>` block it is in.
    fn entry_of(pref: &str, key: &str) -> Option<(String, String)> {
        let at = pref.find(&format!("<entry key=\"{key}\""))?;
        let block = pref[..at].rsplit("<preference ").next()?.split("name=\"").nth(1)?.split('"').next()?;
        let rest = &pref[at..];
        Some((rest[rest.find('>')? + 1..rest.find("</entry>")?].to_string(), block.to_string()))
    }

    #[test]
    fn certificate_zip_names_its_client_store_for_tak_aware_too() {
        let (ca, cert, key) = test_chain();
        let p12 = build_client_store("alpha-shared", &key, &cert, &[ca], "s3cret-pass", P12Format::Modern).unwrap();
        let client = ClientStore { file_name: "alpha-shared.p12".into(), p12, password: "s3cret-pass".into() };
        let pref = config_pref(&package(Some(client)));
        // TAK Aware reads the store's location and password without the connection's number;
        // ATAK reads them with it. Both are among the connection's entries: in ATAK's own
        // preferences the unnumbered ones would replace the device's default certificate.
        let connection = "cot_streams".to_string();
        for (key, value) in [("certificateLocation", "cert/alpha-shared.p12"), ("clientPassword", "s3cret-pass")] {
            assert_eq!(entry_of(&pref, key), Some((value.to_string(), connection.clone())), "{key}");
            assert_eq!(entry_of(&pref, &format!("{key}0")), Some((value.to_string(), connection.clone())), "{key}0");
        }
        // Without a certificate there is no store to name.
        let enrollment = config_pref(&package(None));
        assert!(!enrollment.contains("certificateLocation") && !enrollment.contains("clientPassword"));
    }

    #[test]
    fn no_zip_carries_identity_or_group_settings() {
        let (ca, cert, key) = test_chain();
        let p12 = build_client_store("a", &key, &cert, &[ca], "p", P12Format::Modern).unwrap();
        let client = ClientStore { file_name: "a.p12".into(), p12, password: "p".into() };
        for pkg in [package(None), package(Some(client))] {
            let pref = config_pref(&pkg);
            for key in keys(&pref) {
                assert!(ALLOWED_KEYS.contains(&key.as_str()), "unexpected preference {key}");
            }
            let lower = pref.to_lowercase();
            for forbidden in ["callsign", "locationteam", "atakroletype", "team", "role", "group"] {
                assert!(!lower.contains(forbidden), "{forbidden} found in config.pref");
            }
        }
    }

    #[test]
    fn every_zip_turns_on_atak_channels_for_its_server() {
        let (ca, cert, key) = test_chain();
        let p12 = build_client_store("a", &key, &cert, &[ca], "p", P12Format::Modern).unwrap();
        let client = ClientStore { file_name: "a.p12".into(), p12, password: "p".into() };
        for pkg in [package(None), package(Some(client))] {
            let pref = config_pref(&pkg);
            // In ATAK's own preferences, next to the connection widget, as TAK Server sends them.
            let app = &pref[pref.find(r#"name="com.atakmap.app_preferences""#).expect("app preferences")..];
            assert!(app.contains(r#"<entry key="prefs_enable_channels" class="class java.lang.String">true</entry>"#), "{pref}");
            assert!(
                app.contains(r#"<entry key="prefs_enable_channels_host-tak.example.org" class="class java.lang.String">true</entry>"#),
                "the host is the one of connectString0: {pref}"
            );
        }
        assert_eq!(channels_host_pref("10.0.0.5"), "prefs_enable_channels_host-10.0.0.5");
    }

    #[test]
    fn every_zip_asks_for_the_profiles_of_its_server() {
        let pref = config_pref(&package(None));
        let app = &pref[pref.find(r#"name="com.atakmap.app_preferences""#).expect("app preferences")..];
        assert!(app.contains(r#"<entry key="deviceProfileEnableOnConnect" class="class java.lang.Boolean">true</entry>"#), "{pref}");
    }

    #[test]
    fn stores_open_with_their_password_in_both_formats() {
        let (ca, cert, key) = test_chain();
        for format in [P12Format::Modern, P12Format::Legacy] {
            let trust = build_truststore(std::slice::from_ref(&ca), format).unwrap();
            let store = KeyStore::from_pkcs12(&trust, TRUSTSTORE_PASSWORD, Pkcs12ImportPolicy::Relaxed).unwrap();
            assert_eq!(store.entries_len(), 1);
            assert!(store.private_key_chain().is_none(), "a truststore holds no key");

            let client = build_client_store("alpha-shared", &key, &cert, std::slice::from_ref(&ca), "pw", format).unwrap();
            let store = KeyStore::from_pkcs12(&client, "pw", Pkcs12ImportPolicy::Relaxed).unwrap();
            let (alias, chain) = store.private_key_chain().unwrap();
            assert_eq!(alias, "alpha-shared");
            assert_eq!(chain.certs().len(), 2, "certificate and its issuer");
            assert_eq!(chain.certs()[0].as_der(), cert.as_slice());
            assert_eq!(chain.key().as_der(), key.as_slice());
            assert!(KeyStore::from_pkcs12(&client, "wrong", Pkcs12ImportPolicy::Relaxed).is_err());
        }
    }

    #[test]
    fn qr_strings() {
        assert_eq!(itak_qr("TeamTAK", "tak.example.org", 8089), "TeamTAK,tak.example.org,8089,ssl");
        assert_eq!(itak_qr("A, B", "h", 8089), "A  B,h,8089,ssl");
        assert_eq!(
            atak_enroll_uri("tak.example.org", "alpha-shared", "Ab3 d/+&="),
            "tak://com.atakmap.app/enroll?host=tak.example.org&username=alpha-shared&token=Ab3%20d%2F%2B%26%3D"
        );
    }
}
