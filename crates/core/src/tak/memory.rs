//! In-memory TAK Server for unit tests: it records what it is asked and keeps a list of
//! certificates and sessions that tests can prepare.

use std::sync::Mutex;

use async_trait::async_trait;
use time::{Duration, OffsetDateTime};

use super::{EnrolledCertificate, IssuedCertificate, Subscription, TakError, TakServer};

#[derive(Default)]
struct State {
    certificates: Vec<IssuedCertificate>,
    subscriptions: Vec<Subscription>,
    calls: Vec<String>,
    next_id: i64,
    down: bool,
}

#[derive(Default)]
pub struct MemoryTakServer {
    state: Mutex<State>,
}

impl MemoryTakServer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_unavailable(&self, down: bool) {
        self.state.lock().unwrap().down = down;
    }

    /// Adds an active certificate, as if a device had enrolled by itself.
    pub fn add_certificate(&self, username: &str, fingerprint: &str) -> i64 {
        let mut s = self.state.lock().unwrap();
        s.next_id += 1;
        let id = s.next_id;
        let now = OffsetDateTime::now_utc();
        s.certificates.push(IssuedCertificate {
            id,
            username: username.into(),
            fingerprint: fingerprint.into(),
            client_uid: None,
            issued_at: Some(now),
            expires_at: Some(now + Duration::days(365)),
        });
        id
    }

    pub fn add_subscription(&self, uid: &str, username: &str, groups: &[&str]) {
        self.state.lock().unwrap().subscriptions.push(Subscription {
            subscription_uid: uid.into(),
            username: username.into(),
            callsign: None,
            client_uid: None,
            groups: groups.iter().map(|g| g.to_string()).collect(),
            ..Default::default()
        });
    }

    pub fn certificates(&self) -> Vec<IssuedCertificate> {
        self.state.lock().unwrap().certificates.clone()
    }

    pub fn subscription_uids(&self) -> Vec<String> {
        self.state.lock().unwrap().subscriptions.iter().map(|s| s.subscription_uid.clone()).collect()
    }

    /// Every call received, in order (for example `refresh_groups alice`).
    pub fn calls(&self) -> Vec<String> {
        self.state.lock().unwrap().calls.clone()
    }

    pub fn clear_calls(&self) {
        self.state.lock().unwrap().calls.clear();
    }

    fn enter(&self, call: String) -> Result<std::sync::MutexGuard<'_, State>, TakError> {
        let mut s = self.state.lock().unwrap();
        if s.down {
            return Err(TakError::Unavailable("memory TAK Server set unavailable".into()));
        }
        s.calls.push(call);
        Ok(s)
    }
}

#[async_trait]
impl TakServer for MemoryTakServer {
    async fn version(&self) -> Result<String, TakError> {
        drop(self.enter("version".into())?);
        Ok("memory".into())
    }

    async fn refresh_groups(&self, username: &str) -> Result<(), TakError> {
        self.enter(format!("refresh_groups {username}")).map(|_| ())
    }

    async fn subscriptions(&self) -> Result<Vec<Subscription>, TakError> {
        Ok(self.enter("subscriptions".into())?.subscriptions.clone())
    }

    async fn cut_subscription(&self, subscription_uid: &str) -> Result<(), TakError> {
        let mut s = self.enter(format!("cut_subscription {subscription_uid}"))?;
        s.subscriptions.retain(|x| x.subscription_uid != subscription_uid);
        Ok(())
    }

    async fn active_certificates(&self) -> Result<Vec<IssuedCertificate>, TakError> {
        Ok(self.enter("active_certificates".into())?.certificates.clone())
    }

    async fn revoke_certificate(&self, id: i64) -> Result<(), TakError> {
        let mut s = self.enter(format!("revoke_certificate {id}"))?;
        let username = s.certificates.iter().find(|c| c.id == id).map(|c| c.username.clone());
        s.certificates.retain(|c| c.id != id);
        // TAK Server closes the connections of a revoked certificate; with one certificate
        // per account in these tests, that means the account's sessions.
        if let Some(username) = username {
            if !s.certificates.iter().any(|c| c.username == username) {
                s.subscriptions.retain(|x| x.username != username);
            }
        }
        Ok(())
    }

    async fn enroll(&self, username: &str, _password: &str, client_uid: &str) -> Result<EnrolledCertificate, TakError> {
        let mut s = self.enter(format!("enroll {username}"))?;
        s.next_id += 1;
        let id = s.next_id;
        let now = OffsetDateTime::now_utc();
        let certificate_der = format!("fake certificate {id} for {username}").into_bytes();
        let fingerprint = super::fingerprint(&certificate_der);
        s.certificates.push(IssuedCertificate {
            id,
            username: username.into(),
            fingerprint: fingerprint.clone(),
            client_uid: Some(client_uid.into()),
            issued_at: Some(now),
            expires_at: Some(now + Duration::days(365)),
        });
        Ok(EnrolledCertificate {
            certificate_der,
            private_key_der: b"fake key".to_vec(),
            ca_ders: vec![b"fake ca".to_vec()],
            fingerprint,
            not_before: now,
            not_after: now + Duration::days(365),
        })
    }
}
