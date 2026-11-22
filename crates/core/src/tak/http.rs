//! TAK Server 5.8 over HTTPS, with the calls validated in phase 0:
//!
//! - port 8443, client certificate declared administrator: groups, sessions, certificates,
//!   video feeds, device profiles;
//! - port 8446, HTTP Basic with the directory account: certificate enrollment.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use serde_json::Value;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use super::enroll;
use super::{
    normalize_fingerprint, DeviceProfile, EnrolledCertificate, IssuedCertificate, ProfileFile, ProfileKind, Subscription, TakError,
    TakServer, VideoFeed,
};

#[derive(Clone)]
pub struct HttpTakConfig {
    /// For example `https://takserver:8443`.
    pub api_url: String,
    /// For example `https://takserver:8446`.
    pub enroll_url: String,
    /// CA certificates that sign TAK Server's own certificate (root and intermediate), PEM.
    pub ca_pem: Vec<u8>,
    /// The platform's client certificate, declared administrator in TAK Server, PEM.
    pub client_cert_pem: Vec<u8>,
    /// Its private key, unencrypted PEM.
    pub client_key_pem: Vec<u8>,
    /// Value of the `version` parameter of enrollment requests, shown by TAK Server next to
    /// the certificate.
    pub client_version: String,
}

pub struct HttpTakServer {
    config: HttpTakConfig,
    api: reqwest::Client,
    enroll: reqwest::Client,
}

fn crypto(e: impl std::fmt::Display) -> TakError {
    TakError::Crypto(e.to_string())
}

fn unavailable(e: reqwest::Error) -> TakError {
    TakError::Unavailable(format!("{e}{}", e.source_chain()))
}

trait SourceChain {
    fn source_chain(&self) -> String;
}

impl SourceChain for reqwest::Error {
    fn source_chain(&self) -> String {
        let mut out = String::new();
        let mut source = std::error::Error::source(self);
        while let Some(e) = source {
            out.push_str(&format!(": {e}"));
            source = e.source();
        }
        out
    }
}

/// Characters that may appear in a path segment sent to TAK Server. Platform usernames and
/// subscription identifiers (`tls:12`) only use these; anything else is percent-encoded.
fn encode_segment(segment: &str) -> String {
    let mut out = String::new();
    for b in segment.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~:".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

impl HttpTakServer {
    pub fn new(config: HttpTakConfig) -> Result<Self, TakError> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());

        let mut roots = rustls::RootCertStore::empty();
        let mut private = 0;
        for cert in CertificateDer::pem_slice_iter(&config.ca_pem) {
            roots.add(cert.map_err(crypto)?).map_err(crypto)?;
            private += 1;
        }
        if private == 0 {
            return Err(TakError::Crypto("no CA certificate found in the TAK Server CA file".into()));
        }

        // 8443: private CA only, with the platform's administrator certificate.
        let chain: Vec<CertificateDer<'static>> =
            CertificateDer::pem_slice_iter(&config.client_cert_pem).collect::<Result<_, _>>().map_err(crypto)?;
        let key = PrivateKeyDer::from_pem_slice(&config.client_key_pem).map_err(crypto)?;
        let api_tls = rustls::ClientConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .map_err(crypto)?
            .with_root_certificates(roots.clone())
            .with_client_auth_cert(chain, key)
            .map_err(crypto)?;

        // 8446: the private CA, or a public one when the connector presents a Let's Encrypt
        // certificate (QR enrollment). No client certificate.
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let enroll_tls = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(crypto)?
            .with_root_certificates(roots)
            .with_no_client_auth();

        let build = |tls: rustls::ClientConfig| {
            reqwest::Client::builder()
                .tls_backend_preconfigured(tls)
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(30))
                .build()
                .map_err(crypto)
        };
        Ok(Self { api: build(api_tls)?, enroll: build(enroll_tls)?, config })
    }

    async fn api_call(&self, method: reqwest::Method, path: &str) -> Result<reqwest::Response, TakError> {
        let url = format!("{}{}", self.config.api_url, path);
        let response =
            self.api.request(method.clone(), &url).header("Accept", "application/json").send().await.map_err(unavailable)?;
        if !response.status().is_success() {
            return Err(TakError::Status { call: format!("{method} {path}"), status: response.status().as_u16() });
        }
        Ok(response)
    }

    /// A call with a JSON body.
    async fn api_send(&self, method: reqwest::Method, path: &str, body: &Value) -> Result<(), TakError> {
        let url = format!("{}{}", self.config.api_url, path);
        let response = self.api.request(method.clone(), &url).json(body).send().await.map_err(unavailable)?;
        if !response.status().is_success() {
            return Err(TakError::Status { call: format!("{method} {path}"), status: response.status().as_u16() });
        }
        Ok(())
    }

    /// A call with a raw body.
    async fn api_send_bytes(&self, method: reqwest::Method, path: &str, body: Vec<u8>) -> Result<(), TakError> {
        let url = format!("{}{}", self.config.api_url, path);
        let response = self
            .api
            .request(method.clone(), &url)
            .header("Content-Type", "application/octet-stream")
            .body(body)
            .send()
            .await
            .map_err(unavailable)?;
        if !response.status().is_success() {
            return Err(TakError::Status { call: format!("{method} {path}"), status: response.status().as_u16() });
        }
        Ok(())
    }

    async fn api_data(&self, path: &str) -> Result<Vec<Value>, TakError> {
        let body: Value = self.api_call(reqwest::Method::GET, path).await?.json().await.map_err(unavailable)?;
        match body.get("data") {
            Some(Value::Array(items)) => Ok(items.clone()),
            Some(Value::Null) | None => Ok(Vec::new()),
            Some(other) => Err(TakError::Protocol(format!("{path}: data is not a list: {other}"))),
        }
    }
}

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).filter(|s| !s.is_empty()).map(str::to_string)
}

fn date(v: &Value, key: &str) -> Option<OffsetDateTime> {
    match v.get(key)? {
        Value::String(s) => OffsetDateTime::parse(s, &Rfc3339).ok(),
        Value::Number(n) => n.as_i64().and_then(|ms| OffsetDateTime::from_unix_timestamp_nanos(ms as i128 * 1_000_000).ok()),
        _ => None,
    }
}

/// Reads one element of `certadmin/cert`, whatever its state.
pub fn parse_certificate(v: &Value) -> Option<IssuedCertificate> {
    Some(IssuedCertificate {
        id: v.get("id")?.as_i64()?,
        username: text(v, "userDn")?,
        fingerprint: normalize_fingerprint(&text(v, "hash")?),
        client_uid: text(v, "clientUid"),
        issued_at: date(v, "issuanceDate"),
        expires_at: date(v, "expirationDate"),
    })
}

/// True when TAK Server still accepts the certificate: neither revoked nor expired.
pub fn is_usable(v: &Value, now: OffsetDateTime) -> bool {
    let revoked = v.get("revocationDate").is_some_and(|d| !d.is_null());
    let expired = date(v, "expirationDate").is_some_and(|expiry| expiry <= now);
    !revoked && !expired
}

/// Reads the answer of `GET /Marti/api/video` (video connection manager, version 2): one
/// feed per collection, the first one, which is how the platform registers them.
pub fn parse_video_collections(body: &Value) -> Result<Vec<VideoFeed>, TakError> {
    let items = match body.get("videoConnections") {
        Some(Value::Array(items)) => items,
        Some(Value::Null) | None => return Ok(Vec::new()),
        Some(other) => return Err(TakError::Protocol(format!("/Marti/api/video: videoConnections is not a list: {other}"))),
    };
    Ok(items
        .iter()
        .filter_map(|collection| {
            let feed = collection.get("feeds").and_then(|f| f.as_array()).and_then(|f| f.first());
            Some(VideoFeed {
                uid: text(collection, "uuid")?,
                alias: text(collection, "alias").unwrap_or_default(),
                url: feed.and_then(|f| text(f, "url")).unwrap_or_default(),
            })
        })
        .collect())
}

/// A video collection holding one feed, as `POST` and `PUT /Marti/api/video` take it. The
/// feed asks ATAK for RTSP over TCP (`rtspReliable`): the video server only takes TCP.
pub fn video_collection(feed: &VideoFeed) -> Value {
    serde_json::json!({
        "uuid": feed.uid,
        "active": true,
        "alias": feed.alias,
        "feeds": [{
            "uuid": feed.uid,
            "active": true,
            "alias": feed.alias,
            "url": feed.url,
            "rtspReliable": "1",
        }],
    })
}

/// Reads one element of `GET /Marti/api/device/profile`.
pub fn parse_device_profile(v: &Value) -> Option<DeviceProfile> {
    let groups = v.get("groups").and_then(|g| g.as_array()).map(|items| items.iter().filter_map(|g| g.as_str()).map(str::to_string).collect());
    Some(DeviceProfile {
        id: v.get("id")?.as_i64()?,
        name: text(v, "name")?,
        kind: text(v, "type").as_deref().and_then(ProfileKind::from_tak_type),
        active: v.get("active").and_then(|a| a.as_bool()).unwrap_or(false),
        groups: groups.unwrap_or_default(),
    })
}

/// The body of `PUT /Marti/api/device/profile/{name}`. TAK Server sets the profile's groups
/// from `groups`, and when it is applied from `type`: a profile is applied at enrollment or
/// at connection, not both.
pub fn device_profile_body(id: i64, name: &str, kind: ProfileKind, groups: &[String]) -> Value {
    serde_json::json!({ "id": id, "name": name, "active": true, "type": kind.as_tak_type(), "tool": null, "groups": groups })
}

/// The files of the data package TAK Server makes of a profile
/// (`GET /Marti/api/device/profile/{name}/missionpackage`): file `i` of the profile, in the
/// order of `GET …/files`, is at `file<i>/<name>`.
pub fn read_profile_package(zip_bytes: &[u8]) -> Result<std::collections::BTreeMap<usize, (String, Vec<u8>)>, TakError> {
    use std::io::Read;
    let bad = |e: &dyn std::fmt::Display| TakError::Protocol(format!("profile package: {e}"));
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes)).map_err(|e| bad(&e))?;
    let mut files = std::collections::BTreeMap::new();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| bad(&e))?;
        let path = entry.name().map_err(|e| bad(&e))?.to_string();
        let Some((dir, name)) = path.split_once('/') else { continue };
        let Some(index) = dir.strip_prefix("file").and_then(|n| n.parse::<usize>().ok()) else { continue };
        if entry.is_dir() || name.is_empty() {
            continue;
        }
        let mut data = Vec::new();
        entry.read_to_end(&mut data).map_err(|e| bad(&e))?;
        files.insert(index, (name.to_string(), data));
    }
    Ok(files)
}

/// Reads one element of `subscriptions/all`.
pub fn parse_subscription(v: &Value) -> Option<Subscription> {
    let groups = v
        .get("groups")
        .and_then(|g| g.as_array())
        .map(|items| {
            let mut names: Vec<String> = items.iter().filter_map(|g| text(g, "name")).collect();
            names.sort();
            names.dedup();
            names
        })
        .unwrap_or_default();
    // TAK Server writes "unknown" where a client said nothing about itself.
    let said = |key: &str| text(v, key).map(|s| s.trim().to_string()).filter(|s| !s.is_empty() && s != "unknown");
    let client = match (said("takClient"), said("takVersion")) {
        (Some(name), Some(version)) => Some(format!("{name} {version}")),
        (Some(name), None) => Some(name),
        (None, version) => version,
    };
    Some(Subscription {
        subscription_uid: text(v, "subscriptionUid")?,
        username: text(v, "username").unwrap_or_default(),
        callsign: text(v, "callsign"),
        client_uid: text(v, "clientUid"),
        groups,
        last_report_at: date(v, "lastReportMilliseconds"),
        ip_address: said("ipAddress"),
        client,
    })
}

#[async_trait]
impl TakServer for HttpTakServer {
    async fn version(&self) -> Result<String, TakError> {
        let body = self.api_call(reqwest::Method::GET, "/Marti/api/version").await?.text().await.map_err(unavailable)?;
        Ok(body.trim().trim_matches('"').to_string())
    }

    async fn refresh_groups(&self, username: &str) -> Result<(), TakError> {
        let path = format!("/Marti/api/groups/update/{}", encode_segment(username));
        self.api_call(reqwest::Method::GET, &path).await.map(|_| ())
    }

    async fn subscriptions(&self) -> Result<Vec<Subscription>, TakError> {
        let items = self.api_data("/Marti/api/subscriptions/all").await?;
        Ok(items.iter().filter_map(parse_subscription).collect())
    }

    async fn cut_subscription(&self, subscription_uid: &str) -> Result<(), TakError> {
        let path = format!("/Marti/api/subscriptions/delete/{}", encode_segment(subscription_uid));
        self.api_call(reqwest::Method::DELETE, &path).await.map(|_| ())
    }

    async fn active_certificates(&self) -> Result<Vec<IssuedCertificate>, TakError> {
        // Not `certadmin/cert/active`: on TAK Server 5.8 that list is "the latest certificate
        // of each user and device", so it still shows a revoked certificate when it is the
        // latest, and hides an older one that was never revoked and still works. The full
        // list is read instead, and filtered here.
        let items = self.api_data("/Marti/api/certadmin/cert").await?;
        let now = OffsetDateTime::now_utc();
        Ok(items.iter().filter(|v| is_usable(v, now)).filter_map(parse_certificate).collect())
    }

    async fn revoke_certificate(&self, id: i64) -> Result<(), TakError> {
        self.api_call(reqwest::Method::DELETE, &format!("/Marti/api/certadmin/cert/revoke/{id}")).await.map(|_| ())
    }

    async fn enroll(&self, username: &str, password: &str, client_uid: &str) -> Result<EnrolledCertificate, TakError> {
        let config_url = format!("{}/Marti/api/tls/config", self.config.enroll_url);
        let response = self.enroll.get(&config_url).basic_auth(username, Some(password)).send().await.map_err(unavailable)?;
        if !response.status().is_success() {
            return Err(TakError::Status { call: "GET /Marti/api/tls/config".into(), status: response.status().as_u16() });
        }
        let name_entries = enroll::parse_name_entries(&response.text().await.map_err(unavailable)?);

        let cn = username.to_string();
        let made =
            tokio::task::spawn_blocking(move || enroll::generate_key_and_csr(&name_entries, &cn)).await.map_err(crypto)??;

        let sign_url = format!(
            "{}/Marti/api/tls/signClient/v2?clientUid={}&version={}",
            self.config.enroll_url,
            encode_segment(client_uid),
            encode_segment(&self.config.client_version)
        );
        let response = self
            .enroll
            .post(&sign_url)
            .basic_auth(username, Some(password))
            .header("Content-Type", "text/plain")
            .header("Accept", "application/json")
            .body(enroll::csr_body(&made.csr_der))
            .send()
            .await
            .map_err(unavailable)?;
        if !response.status().is_success() {
            return Err(TakError::Status {
                call: "POST /Marti/api/tls/signClient/v2".into(),
                status: response.status().as_u16(),
            });
        }
        let body: Value = response.json().await.map_err(unavailable)?;
        enroll::read_signed_response(username, made.private_key_der, &body)
    }

    async fn video_feeds(&self) -> Result<Vec<VideoFeed>, TakError> {
        let body: Value = self.api_call(reqwest::Method::GET, "/Marti/api/video").await?.json().await.map_err(unavailable)?;
        parse_video_collections(&body)
    }

    async fn add_video_feed(&self, feed: &VideoFeed, group: &str) -> Result<(), TakError> {
        let path = format!("/Marti/api/video?group={}", encode_segment(group));
        self.api_send(reqwest::Method::POST, &path, &serde_json::json!({ "videoConnections": [video_collection(feed)] })).await
    }

    async fn update_video_feed(&self, feed: &VideoFeed) -> Result<(), TakError> {
        let path = format!("/Marti/api/video/{}", encode_segment(&feed.uid));
        self.api_send(reqwest::Method::PUT, &path, &video_collection(feed)).await
    }

    async fn delete_video_feed(&self, uid: &str) -> Result<(), TakError> {
        self.api_call(reqwest::Method::DELETE, &format!("/Marti/api/video/{}", encode_segment(uid))).await.map(|_| ())
    }

    async fn device_profiles(&self) -> Result<Vec<DeviceProfile>, TakError> {
        let items = self.api_data("/Marti/api/device/profile").await?;
        Ok(items.iter().filter_map(parse_device_profile).collect())
    }

    async fn profile_files(&self, profile: &str) -> Result<Vec<ProfileFile>, TakError> {
        // The list gives identifiers and names, the profile's data package the contents: two
        // calls, whatever the number of files.
        let base = format!("/Marti/api/device/profile/{}", encode_segment(profile));
        let listed = self.api_data(&format!("{base}/files")).await?;
        let listed: Vec<(i64, String)> =
            listed.iter().filter_map(|f| Some((f.get("id")?.as_i64()?, text(f, "name")?))).collect();
        if listed.is_empty() {
            return Ok(Vec::new());
        }
        let package = self.api_call(reqwest::Method::GET, &format!("{base}/missionpackage")).await?;
        let package = read_profile_package(&package.bytes().await.map_err(unavailable)?)?;
        let mut files = Vec::with_capacity(listed.len());
        for (index, (id, name)) in listed.into_iter().enumerate() {
            let data = match package.get(&index) {
                Some((packed, data)) if *packed == name => data.clone(),
                // Not where expected (a file added meanwhile): asked for alone.
                _ => {
                    let path = format!("{base}/file/{id}");
                    self.api_call(reqwest::Method::GET, &path).await?.bytes().await.map_err(unavailable)?.to_vec()
                }
            };
            files.push(ProfileFile { id, name, data });
        }
        Ok(files)
    }

    async fn create_device_profile(&self, name: &str, groups: &[String]) -> Result<(), TakError> {
        let query: Vec<String> = groups.iter().map(|g| format!("group={}", encode_segment(g))).collect();
        let path = format!("/Marti/api/device/profile/{}?{}", encode_segment(name), query.join("&"));
        self.api_call(reqwest::Method::POST, &path).await.map(|_| ())
    }

    async fn set_device_profile(&self, id: i64, name: &str, kind: ProfileKind, groups: &[String]) -> Result<(), TakError> {
        let path = format!("/Marti/api/device/profile/{}", encode_segment(name));
        self.api_send(reqwest::Method::PUT, &path, &device_profile_body(id, name, kind, groups)).await
    }

    async fn add_profile_file(&self, profile: &str, file_name: &str, data: &[u8]) -> Result<(), TakError> {
        let path = format!("/Marti/api/device/profile/{}/file?filename={}", encode_segment(profile), encode_segment(file_name));
        self.api_send_bytes(reqwest::Method::PUT, &path, data.to_vec()).await
    }

    async fn delete_profile_file(&self, profile: &str, file_id: i64) -> Result<(), TakError> {
        let path = format!("/Marti/api/device/profile/{}/file/{file_id}", encode_segment(profile));
        self.api_call(reqwest::Method::DELETE, &path).await.map(|_| ())
    }

    async fn delete_device_profile(&self, id: i64) -> Result<(), TakError> {
        self.api_call(reqwest::Method::DELETE, &format!("/Marti/api/device/profile/{id}")).await.map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn certificates_are_read_as_the_server_lists_them() {
        // Shape observed on TAK Server 5.8-RELEASE-103 (phase 0, question 4).
        let v = json!({"id": 7, "userDn": "dave", "clientUid": "spike-dave", "serialNumber": "7c8b879b",
            "hash": "AB:CD:EF:01", "issuanceDate": "2026-11-19T14:49:32.461Z",
            "expirationDate": "2026-12-19T14:49:32.460Z", "revocationDate": null});
        let c = parse_certificate(&v).unwrap();
        assert_eq!((c.id, c.username.as_str(), c.fingerprint.as_str()), (7, "dave", "abcdef01"));
        assert_eq!(c.client_uid.as_deref(), Some("spike-dave"));
        assert_eq!(c.expires_at.unwrap().unix_timestamp(), 1797691772);
        assert!(parse_certificate(&json!({"id": 1})).is_none());
    }

    #[test]
    fn revoked_and_expired_certificates_are_not_active() {
        let now = OffsetDateTime::parse("2026-11-20T00:00:00Z", &Rfc3339).unwrap();
        let cert = |revoked: Value, expires: &str| json!({"id": 1, "userDn": "u", "hash": "ab", "revocationDate": revoked, "expirationDate": expires});
        assert!(is_usable(&cert(Value::Null, "2027-11-19T15:49:32.952Z"), now));
        // Shape returned by TAK Server for a revoked certificate, which it still lists under
        // certadmin/cert/active when it is the latest one of its user and device.
        assert!(!is_usable(&cert(json!("2026-11-19T15:50:15.906Z"), "2027-11-19T15:49:32.952Z"), now));
        assert!(!is_usable(&cert(Value::Null, "2026-11-19T15:49:32.952Z"), now));
        assert!(
            is_usable(&json!({"id": 1, "userDn": "u", "hash": "ab"}), now),
            "no dates: assume usable, so it gets revoked when it should"
        );
    }

    #[test]
    fn subscriptions_are_read_as_the_server_lists_them() {
        let v = json!({"subscriptionUid": "tls:11", "username": "frank", "callsign": "FRANK", "clientUid": "U1",
            "groups": [{"name": "team-bravo", "direction": "IN"}, {"name": "team-bravo", "direction": "OUT"}]});
        let s = parse_subscription(&v).unwrap();
        assert_eq!(s.subscription_uid, "tls:11");
        assert_eq!(s.groups, vec!["team-bravo".to_string()]);
        let none = parse_subscription(&json!({"subscriptionUid": "tls:12", "username": "x", "groups": []})).unwrap();
        assert!(none.groups.is_empty());
        assert_eq!((none.last_report_at, none.ip_address, none.client), (None, None, None));
    }

    #[test]
    fn what_a_device_says_about_itself_is_read() {
        // Shapes observed on TAK Server 5.8-RELEASE-103: a phone, and a session without any
        // group, which the server lists without its account.
        let phone = json!({"subscriptionUid": "tls:21", "username": "alpha-shared", "callsign": "ALPHA-1", "clientUid": "ANDROID-1",
            "lastReportMilliseconds": 1795108155112i64, "takClient": "ATAK-CIV", "takVersion": "5.4.0.2", "ipAddress": " 192.168.186.1",
            "groups": [{"name": "alpha", "direction": "IN"}]});
        let s = parse_subscription(&phone).unwrap();
        assert_eq!(s.client.as_deref(), Some("ATAK-CIV 5.4.0.2"));
        assert_eq!(s.ip_address.as_deref(), Some("192.168.186.1"), "the server pads the address with a space");
        assert_eq!(s.last_report_at.unwrap().unix_timestamp(), 1795108155);
        let ghost = json!({"subscriptionUid": "tls:3", "username": null, "dn": null, "callsign": "E2E-x", "takClient": "unknown",
            "takVersion": "", "role": "unknown", "ipAddress": "", "groups": []});
        let s = parse_subscription(&ghost).unwrap();
        assert_eq!((s.username.as_str(), s.client, s.ip_address), ("", None, None));
    }

    #[test]
    fn video_feeds_are_read_as_the_server_lists_them() {
        // Shape observed on TAK Server 5.8-RELEASE-103.
        let body = json!({"videoConnections": [{"active": true, "alias": "Drone 1 · Alpha", "thumbnail": null, "classification": null,
            "feeds": [{"uuid": "teamtak-video-1", "active": true, "alias": "Drone 1 · Alpha", "url": "rtsp://view:k@h:8554/alpha/drone-1",
                "order": null, "macAddress": null, "rtspReliable": "1"}],
            "uuid": "teamtak-video-1"}, {"uuid": "empty-collection", "alias": "x", "feeds": []}, {"alias": "no uid"}]});
        let feeds = parse_video_collections(&body).unwrap();
        assert_eq!(feeds.len(), 2);
        assert_eq!(feeds[0], VideoFeed { uid: "teamtak-video-1".into(), alias: "Drone 1 · Alpha".into(), url: "rtsp://view:k@h:8554/alpha/drone-1".into() });
        assert_eq!(feeds[1].url, "", "a collection without feed is listed, so that it can be removed");
        assert_eq!(parse_video_collections(&json!({"videoConnections": []})).unwrap(), vec![]);
        assert!(parse_video_collections(&json!({"videoConnections": 3})).is_err());

        let sent = video_collection(&feeds[0]);
        assert_eq!(sent["uuid"], "teamtak-video-1");
        assert_eq!(sent["feeds"][0]["url"], "rtsp://view:k@h:8554/alpha/drone-1");
        assert_eq!(sent["feeds"][0]["rtspReliable"], "1");
        assert_eq!(parse_video_collections(&json!({"videoConnections": [sent]})).unwrap(), vec![feeds[0].clone()], "read back as written");
    }

    #[test]
    fn device_profiles_are_read_as_the_server_lists_them() {
        // Shape observed on TAK Server 5.8-RELEASE-103.
        let v = json!({"id": 1, "name": "teamtak-maps-alpha", "active": true, "applyOnEnrollment": false, "applyOnConnect": true,
            "type": "Connection", "updated": 1795287469274i64, "tool": null, "groups": ["evt-exercise", "alpha"]});
        let p = parse_device_profile(&v).unwrap();
        assert_eq!((p.id, p.name.as_str(), p.kind, p.active), (1, "teamtak-maps-alpha", Some(ProfileKind::Connection), true));
        assert_eq!(p.groups.iter().map(String::as_str).collect::<Vec<_>>(), ["alpha", "evt-exercise"]);
        let tool = parse_device_profile(&json!({"id": 2, "name": "t", "active": false, "type": "Tool", "groups": []})).unwrap();
        assert_eq!((tool.kind, tool.active), (None, false));
        let body = device_profile_body(1, "teamtak-enrollment", ProfileKind::Enrollment, &["alpha".into()]);
        assert_eq!(body["type"], "Enrollment");
        assert_eq!(body["groups"], json!(["alpha"]));
        assert_eq!(body["active"], true);
    }

    #[test]
    fn a_profile_package_is_read_by_file_index() {
        use std::io::Write;
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        // The layout of TAK Server 5.8's profile packages.
        for (path, data) in [("file0/", ""), ("file0/a.xml", "A"), ("file1/", ""), ("file1/b.xml", "B"), ("MANIFEST/manifest.xml", "<m/>")] {
            if path.ends_with('/') {
                zip.add_directory(path, options).unwrap();
            } else {
                zip.start_file(path, options).unwrap();
                zip.write_all(data.as_bytes()).unwrap();
            }
        }
        let files = read_profile_package(&zip.finish().unwrap().into_inner()).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[&0], ("a.xml".to_string(), b"A".to_vec()));
        assert_eq!(files[&1], ("b.xml".to_string(), b"B".to_vec()));
        assert!(read_profile_package(b"not a zip").is_err());
    }

    #[test]
    fn path_segments_are_encoded() {
        assert_eq!(encode_segment("alpha-shared"), "alpha-shared");
        assert_eq!(encode_segment("tls:11"), "tls:11");
        assert_eq!(encode_segment("a/b ?"), "a%2Fb%20%3F");
    }
}
