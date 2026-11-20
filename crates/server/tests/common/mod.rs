//! Helpers of the integration tests: an API client per signed-in account, the mail catcher,
//! the directory seen from inside its container, and a scripted TAK client.
//!
//! The tests are black-box: they drive the platform through its public HTTP API on the
//! running stack (`./install.sh --local`), read what it wrote to OpenLDAP, and connect to
//! TAK Server's client port (8089 by default), enrollment port (8446) and API port (8443) the
//! way a phone does.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Once};
use std::time::{Duration, Instant};

use p12_keystore::{KeyStore, KeyStoreEntry, Pkcs12ImportPolicy};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Name TAK Server's certificate is checked against when connecting to 127.0.0.1.
const TAK_SERVER_NAME: &str = "localhost";

pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

/// Settings of the running stack, read from deploy/.env.
pub fn env() -> &'static BTreeMap<String, String> {
    static ENV: std::sync::OnceLock<BTreeMap<String, String>> = std::sync::OnceLock::new();
    ENV.get_or_init(|| {
        let path = repo_root().join("deploy/.env");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{}: {e}. Start the stack first: ./install.sh --local", path.display()));
        text.lines()
            .filter(|l| !l.starts_with('#'))
            .filter_map(|l| l.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    })
}

pub fn setting(name: &str) -> &'static str {
    env().get(name).unwrap_or_else(|| panic!("{name} missing from deploy/.env"))
}

/// Ports of the machine TAK clients connect to, the main one first (TAK_CLIENT_PORTS).
pub fn client_ports() -> Vec<u16> {
    let raw = env().get("TAK_CLIENT_PORTS").map(String::as_str).unwrap_or("8089");
    raw.split([',', ' ']).filter(|s| !s.is_empty()).map(|p| p.parse().unwrap()).collect()
}

pub fn base_url() -> String {
    std::env::var("TM_BASE_URL").unwrap_or_else(|_| "http://127.0.0.1:8480".to_string())
}

pub fn mailpit_url() -> String {
    std::env::var("TM_MAILPIT_URL").unwrap_or_else(|_| "http://127.0.0.1:8025".to_string())
}

pub fn install_crypto() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
        assert_eq!(setting("MODE"), "local", "the integration tests need the local mode (mail catcher)");
    });
}

/// A name no earlier run has used, so tests can be replayed on the same stack.
pub fn unique(prefix: &str) -> String {
    format!("{prefix}{}", teamtak_core::random_string(6, b"abcdefghijkmnpqrstuvwxyz23456789"))
}

pub fn email(prefix: &str) -> String {
    format!("{}@e2e.teamtak.test", unique(prefix))
}

// ------------------------------------------------------------------------------- compose

/// Runs `docker compose exec` in a service of the stack and returns its output.
pub fn compose_exec(service: &str, args: &[&str], stdin: Option<&str>) -> (bool, String) {
    use std::io::Write;
    let deploy = repo_root().join("deploy");
    let mut command = Command::new("docker");
    command
        .current_dir(&deploy)
        .args(["compose", "exec", "-T", service])
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = command.spawn().expect("docker compose exec");
    if let Some(input) = stdin {
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    }
    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    let text = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    (output.status.success(), text)
}

/// Runs `docker compose <args>` on the stack and returns its output.
pub fn compose(args: &[&str]) -> (bool, String) {
    let output = Command::new("docker").current_dir(repo_root().join("deploy")).arg("compose").args(args).output().expect("docker compose");
    let text = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    (output.status.success(), text)
}

/// Stops the platform's container. OpenLDAP, TAK Server and the connected TAK clients stay.
pub fn stop_platform() {
    let (ok, output) = compose(&["stop", "app"]);
    assert!(ok, "stopping the platform failed: {output}");
}

/// Starts the platform's container again and waits until it answers through the proxy.
pub async fn start_platform() {
    let (ok, output) = compose(&["start", "app"]);
    assert!(ok, "starting the platform failed: {output}");
    let http = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let answer = http.get(format!("{}/api/health", base_url())).send().await;
        if answer.is_ok_and(|r| r.status().is_success()) {
            return;
        }
        assert!(Instant::now() < deadline, "the platform did not come back");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

// ------------------------------------------------------------------------------- settings

/// Platform settings changed for the length of a test. When it is dropped, at the end of the
/// test whatever its outcome, the values from before are written back: a test that failed
/// with sign-ups closed would otherwise leave every following test unable to sign up.
pub struct SettingsChanged {
    before: Value,
}

impl SettingsChanged {
    /// Changes `changes` (fields of the settings) through the API, as an administrator does.
    pub async fn apply(admin: &Session, changes: Value) -> Self {
        let current = admin.get("/admin/settings").await.ok()["settings"].clone();
        let before = changes.as_object().expect("an object of settings").keys().map(|k| (k.clone(), current[k].clone())).collect();
        admin.patch("/admin/settings", json!({ "settings": changes })).await.ok();
        Self { before: Value::Object(before) }
    }
}

impl Drop for SettingsChanged {
    fn drop(&mut self) {
        // In the database itself: a drop cannot wait for the API. The platform reads its
        // settings there at every use.
        let sql = format!("UPDATE settings SET data = data || $json${}$json$::jsonb WHERE id = 1;", self.before);
        let (ok, output) = compose_exec("postgres", &["psql", "-U", "teamtak", "-d", "teamtak", "-v", "ON_ERROR_STOP=1"], Some(&sql));
        if !ok {
            eprintln!("settings not put back ({}): {output}", self.before);
        }
    }
}

// ------------------------------------------------------------------------------- directory

fn manager_dn() -> String {
    format!("cn=teamtak,ou=Services,{}", setting("LDAP_BASE_DN"))
}

/// `ldapsearch` as the platform's account, from inside the openldap container.
pub fn ldap_search(base_rdn: &str, filter: &str, attrs: &[&str]) -> String {
    let base = format!("{base_rdn},{}", setting("LDAP_BASE_DN"));
    let dn = manager_dn();
    let mut args = vec![
        "ldapsearch",
        "-LLL",
        "-x",
        "-o",
        "ldif-wrap=no",
        "-H",
        "ldap://localhost:389",
        "-D",
        &dn,
        "-w",
        setting("LDAP_MANAGER_PW"),
        "-b",
        &base,
        filter,
    ];
    args.extend_from_slice(attrs);
    compose_exec("openldap", &args, None).1
}

/// Applies an LDIF as the platform's account: how the tests tamper with the directory.
pub fn ldap_modify(ldif: &str) -> (bool, String) {
    let dn = manager_dn();
    compose_exec(
        "openldap",
        &["ldapmodify", "-x", "-H", "ldap://localhost:389", "-D", &dn, "-w", setting("LDAP_MANAGER_PW")],
        Some(ldif),
    )
}

pub fn user_dn(username: &str) -> String {
    format!("uid={username},ou=People,{}", setting("LDAP_BASE_DN"))
}

pub fn group_dn(group: &str) -> String {
    format!("cn={group},ou=Group,{}", setting("LDAP_BASE_DN"))
}

pub fn ldap_user_exists(username: &str) -> bool {
    ldap_search("ou=People", &format!("(uid={username})"), &["uid"]).contains(&format!("uid: {username}"))
}

/// Usernames in a group; empty when the group does not exist.
pub fn ldap_group_members(group: &str) -> Vec<String> {
    let out = ldap_search("ou=Group", &format!("(cn={group})"), &["member"]);
    let mut members: Vec<String> = out
        .lines()
        .filter_map(|l| l.strip_prefix("member: uid="))
        .filter_map(|l| l.split(',').next())
        .map(str::to_string)
        .collect();
    members.sort();
    members
}

/// Groups TAK Server reads for a user (the memberOf attribute).
pub fn ldap_member_of(username: &str) -> Vec<String> {
    let out = ldap_search("ou=People", &format!("(uid={username})"), &["memberOf"]);
    let mut groups: Vec<String> = out
        .lines()
        .filter_map(|l| l.strip_prefix("memberOf: cn="))
        .filter_map(|l| l.split(',').next())
        .map(str::to_string)
        .collect();
    groups.sort();
    groups
}

// ------------------------------------------------------------------------------- mail

pub struct Mail {
    /// Without the server's name (see [`untagged`]).
    pub subject: String,
    pub text: String,
}

/// A subject as it is written when the server has no name: an administrator may have named
/// the server of the test stack (`[TeamTAK · Bench] Alert: …`, `[Bench] Your sign-in code: …`).
pub fn untagged(subject: &str) -> String {
    if let Some(rest) = subject.strip_prefix("[TeamTAK · ") {
        if let Some((_, after)) = rest.split_once(']') {
            return format!("[TeamTAK]{after}");
        }
    }
    if subject.starts_with('[') && !subject.starts_with("[TeamTAK]") {
        if let Some((_, after)) = subject.split_once("] ") {
            return after.to_string();
        }
    }
    subject.to_string()
}

/// Latest message received by `to`, waiting up to ten seconds for it.
pub async fn latest_mail(to: &str) -> Mail {
    let http = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let list: Value = http
            .get(format!("{}/api/v1/search", mailpit_url()))
            .query(&[("query", format!("to:{to}"))])
            .send()
            .await
            .expect("mail catcher reachable")
            .json()
            .await
            .unwrap();
        if let Some(first) = list["messages"].as_array().and_then(|m| m.first()) {
            let id = first["ID"].as_str().unwrap();
            let message: Value =
                http.get(format!("{}/api/v1/message/{id}", mailpit_url())).send().await.unwrap().json().await.unwrap();
            return Mail {
                subject: untagged(message["Subject"].as_str().unwrap_or_default()),
                text: message["Text"].as_str().unwrap_or_default().to_string(),
            };
        }
        assert!(Instant::now() < deadline, "no e-mail received by {to}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// The latest message received by `to` whose subject contains `needle`, waiting up to
/// fifteen seconds for it. Notifications are sent in the background, so the latest message
/// of an address is not always the one a test expects.
pub async fn mail_about(to: &str, needle: &str) -> Mail {
    let http = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let list: Value = http
            .get(format!("{}/api/v1/search", mailpit_url()))
            .query(&[("query", format!("to:{to}")), ("limit", "100".to_string())])
            .send()
            .await
            .expect("mail catcher reachable")
            .json()
            .await
            .unwrap();
        let messages = list["messages"].as_array().cloned().unwrap_or_default();
        if let Some(found) = messages.iter().find(|m| m["Subject"].as_str().unwrap_or_default().contains(needle)) {
            let id = found["ID"].as_str().unwrap();
            let message: Value =
                http.get(format!("{}/api/v1/message/{id}", mailpit_url())).send().await.unwrap().json().await.unwrap();
            return Mail {
                subject: untagged(message["Subject"].as_str().unwrap_or_default()),
                text: message["Text"].as_str().unwrap_or_default().to_string(),
            };
        }
        let subjects: Vec<&str> = messages.iter().filter_map(|m| m["Subject"].as_str()).collect();
        assert!(Instant::now() < deadline, "no e-mail about {needle:?} received by {to}; it has: {subjects:?}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Subjects of every message received by `to`, newest first, as they were received.
pub async fn mail_subjects(to: &str) -> Vec<String> {
    let list: Value = reqwest::Client::new()
        .get(format!("{}/api/v1/search", mailpit_url()))
        .query(&[("query", format!("to:{to}")), ("limit", "100".to_string())])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    list["messages"].as_array().map(|m| m.iter().filter_map(|m| m["Subject"].as_str().map(str::to_string)).collect()).unwrap_or_default()
}

pub async fn mail_count(to: &str) -> u64 {
    let list: Value = reqwest::Client::new()
        .get(format!("{}/api/v1/search", mailpit_url()))
        .query(&[("query", format!("to:{to}"))])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    list["messages_count"].as_u64().or_else(|| list["total"].as_u64()).unwrap_or(0)
}

/// The six digits of a login e-mail.
pub fn code_in(mail: &Mail) -> String {
    let digits: String = mail.subject.chars().filter(|c| c.is_ascii_digit()).collect();
    assert_eq!(digits.len(), 6, "no code in subject {:?}", mail.subject);
    digits
}

// ------------------------------------------------------------------------------- API

/// An HTTP session on the platform (one per account).
#[derive(Clone)]
pub struct Session {
    http: reqwest::Client,
    /// Same cookies, and redirects are not followed: to see where a page sends the browser.
    no_redirect: reqwest::Client,
    pub email: String,
}

pub struct Answer {
    pub status: u16,
    pub body: Value,
}

impl Answer {
    /// The API's error code, when it is an error.
    pub fn error(&self) -> &str {
        self.body["error"].as_str().unwrap_or("")
    }

    #[track_caller]
    pub fn ok(self) -> Value {
        assert!((200..300).contains(&self.status), "HTTP {} {}", self.status, self.body);
        self.body
    }

    #[track_caller]
    pub fn expect_error(&self, status: u16, code: &str) {
        assert_eq!((self.status, self.error()), (status, code), "body: {}", self.body);
    }
}

impl Session {
    /// A browser that has not signed in.
    pub fn anonymous() -> Self {
        install_crypto();
        let jar = Arc::new(reqwest::cookie::Jar::default());
        let http = reqwest::Client::builder().cookie_provider(jar.clone()).timeout(Duration::from_secs(60)).build().unwrap();
        let no_redirect = reqwest::Client::builder()
            .cookie_provider(jar)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(60))
            .build()
            .unwrap();
        Self { http, no_redirect, email: String::new() }
    }

    /// A page outside the API, the way the browser of this session gets it: status, where a
    /// redirect sends it, and the body.
    pub async fn fetch(&self, url: &str) -> (u16, String, String) {
        let response = self.no_redirect.get(url).send().await.unwrap_or_else(|e| panic!("{url}: {e}"));
        let status = response.status().as_u16();
        let location = response.headers().get("location").and_then(|l| l.to_str().ok()).unwrap_or_default().to_string();
        (status, location, response.text().await.unwrap_or_default())
    }

    pub async fn call(&self, method: &str, path: &str, body: Option<Value>) -> Answer {
        let url = format!("{}/api{path}", base_url());
        let mut request = self
            .http
            .request(method.parse().unwrap(), url)
            .header("X-Requested-With", "teamtak")
            .header("Accept", "application/json");
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.expect("platform reachable: is the stack up?");
        let status = response.status().as_u16();
        let bytes = response.bytes().await.unwrap();
        Answer { status, body: serde_json::from_slice(&bytes).unwrap_or(Value::Null) }
    }

    pub async fn get(&self, path: &str) -> Answer {
        self.call("GET", path, None).await
    }

    pub async fn post(&self, path: &str, body: Value) -> Answer {
        self.call("POST", path, Some(body)).await
    }

    pub async fn patch(&self, path: &str, body: Value) -> Answer {
        self.call("PATCH", path, Some(body)).await
    }

    pub async fn delete(&self, path: &str) -> Answer {
        self.call("DELETE", path, None).await
    }

    /// A solved anti-bot challenge, the way the sign-in page makes one in the background.
    pub async fn proof(&self) -> Value {
        let c = self.get("/auth/challenge").await.ok();
        let (salt, challenge) = (c["salt"].as_str().unwrap(), c["challenge"].as_str().unwrap());
        let number = teamtak_core::challenge::solve(salt, challenge, c["max_number"].as_u64().unwrap())
            .unwrap_or_else(|| panic!("no solution to {c}"));
        json!({ "salt": salt, "challenge": challenge, "number": number, "signature": c["signature"] })
    }

    /// Asks once for a login code, with a solved challenge, the way the sign-in page does.
    pub async fn request_code_once(&self, email: &str, language: Option<&str>) -> Answer {
        let mut body = json!({ "email": email, "proof": self.proof().await });
        if let Some(language) = language {
            body["language"] = json!(language);
        }
        self.post("/auth/request-code", body).await
    }

    /// Asks for a login code; when one was sent to the address moments ago, waits for the
    /// countdown the way a user does, and asks again.
    pub async fn request_code(&self, email: &str, language: Option<&str>) -> Answer {
        loop {
            let answer = self.request_code_once(email, language).await;
            if answer.error() != "resend_too_soon" {
                return answer;
            }
            let wait = answer.body["retry_after"].as_u64().unwrap_or_else(|| panic!("no retry_after: {}", answer.body));
            tokio::time::sleep(Duration::from_secs(wait)).await;
        }
    }

    /// Signs in the way a user does: asks for a code, reads it in the mail, submits it.
    pub async fn login(email: &str, language: &str) -> Self {
        let mut session = Self::anonymous();
        session.request_code(email, Some(language)).await.ok();
        let code = code_in(&latest_mail(email).await);
        session.post("/auth/verify", json!({ "email": email, "code": code, "language": language })).await.ok();
        session.email = email.to_string();
        session
    }

    /// A new account, signed in.
    pub async fn new_account(prefix: &str) -> Self {
        Self::login(&email(prefix), "en").await
    }

    /// A platform administrator, obtained with the server-side rescue command (which is how
    /// the first administrator of an installation gets in).
    pub async fn admin() -> Self {
        let address = email("admin");
        let (ok, output) = compose_exec("app", &["teamtak", "grant-admin", &address], None);
        assert!(ok, "grant-admin failed: {output}");
        let code: String = output
            .lines()
            .find(|l| l.contains("One-time login code"))
            .and_then(|l| l.rsplit(':').next())
            .map(|c| c.trim().to_string())
            .unwrap_or_else(|| panic!("no code in: {output}"));
        let mut session = Self::anonymous();
        // While the code waits, the sign-in page offers "I already have a code".
        assert_eq!(session.get("/config").await.ok()["rescue_code_pending"], true);
        session.post("/auth/verify", json!({ "email": address, "code": code })).await.ok();
        session.email = address;
        session
    }

    pub async fn account_id(&self) -> String {
        self.get("/me").await.ok()["account"]["id"].as_str().unwrap().to_string()
    }

    /// Downloads a connection zip.
    pub async fn package(&self, access_id: &str, form: &str) -> Vec<u8> {
        self.package_at(access_id, form, "").await.1
    }

    /// Downloads a connection zip with a query string (`?port=8443`): its file name and its
    /// bytes.
    pub async fn package_at(&self, access_id: &str, form: &str, query: &str) -> (String, Vec<u8>) {
        let url = format!("{}/api/accesses/{access_id}/package/{form}{query}", base_url());
        let response = self.http.get(url).send().await.unwrap();
        assert!(response.status().is_success(), "package {form}: HTTP {}", response.status());
        assert_eq!(response.headers()["content-type"], "application/zip");
        let disposition = response.headers()["content-disposition"].to_str().unwrap();
        let name = disposition.split("filename=\"").nth(1).and_then(|rest| rest.split('"').next()).unwrap().to_string();
        (name, response.bytes().await.unwrap().to_vec())
    }
}

/// A team that went through the whole path: requested by a new account, approved by an
/// administrator.
pub struct TeamFixture {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub leader: Session,
}

pub async fn approved_team(admin: &Session, mode: &str, member_cap: u32) -> TeamFixture {
    let leader = Session::new_account("leader").await;
    let name = unique("E2E ");
    let team = leader.post("/teams", json!({ "name": name, "mode": mode, "message": "integration test" })).await.ok();
    let id = team["id"].as_str().unwrap().to_string();
    admin.post(&format!("/admin/teams/{id}/decision"), json!({ "approve": true, "member_cap": member_cap })).await.ok();
    TeamFixture { id, slug: team["slug"].as_str().unwrap().to_string(), name, leader }
}

/// Invites a new account into a team and makes it accept.
pub async fn add_member(team: &TeamFixture) -> Session {
    let address = email("member");
    team.leader.post(&format!("/teams/{}/invitations", team.id), json!({ "email": address })).await.ok();
    let member = Session::login(&address, "en").await;
    let me = member.get("/me").await.ok();
    let invitation = me["invitations"][0]["id"].as_str().expect("invitation waiting").to_string();
    member.post(&format!("/invitations/{invitation}/answer"), json!({ "accept": true })).await.ok();
    member
}

pub async fn create_access(session: &Session, team_id: &str, body: Value) -> String {
    let created = session.post(&format!("/teams/{team_id}/accesses"), body).await.ok();
    assert_eq!(created["synced"], true, "the access must be live in the directory when the call returns");
    created["id"].as_str().unwrap().to_string()
}

/// Creates an access, downloads its zip with certificate, and returns what a device needs
/// to connect with it.
pub async fn access_identity(session: &Session, team_id: &str, body: Value) -> Identity {
    let access = create_access(session, team_id, body).await;
    Package::open(&session.package(&access, "certificate").await).identity()
}

// ------------------------------------------------------------------------------- events

pub fn iso(at: time::OffsetDateTime) -> String {
    at.format(&time::format_description::well_known::Rfc3339).unwrap()
}

pub fn parse_time(value: &Value) -> time::OffsetDateTime {
    let text = value.as_str().unwrap_or_else(|| panic!("not a date: {value}"));
    time::OffsetDateTime::parse(text, &time::format_description::well_known::Rfc3339).unwrap()
}

pub fn utc_in(seconds: i64) -> time::OffsetDateTime {
    time::OffsetDateTime::now_utc() + time::Duration::seconds(seconds)
}

/// The moment of this process's monotonic clock that corresponds to a date.
pub fn instant_of(at: time::OffsetDateTime) -> Instant {
    let delta = at - time::OffsetDateTime::now_utc();
    let now = Instant::now();
    if delta.is_negative() {
        now.checked_sub(delta.unsigned_abs()).unwrap_or(now)
    } else {
        now + delta.unsigned_abs()
    }
}

pub struct EventFixture {
    pub id: String,
    pub name: String,
    /// Name of the TAK group, `evt-…`.
    pub group: String,
    pub starts_at: time::OffsetDateTime,
    pub ends_at: time::OffsetDateTime,
}

/// A leader creates an event for their team. `starts_in` is `None` for an event that
/// starts at once.
pub async fn create_event(host: &TeamFixture, starts_in: Option<i64>, lasts: i64) -> EventFixture {
    let name = unique("Exercise ");
    let starts_at = starts_in.map(utc_in);
    let ends_at = starts_at.unwrap_or_else(|| utc_in(0)) + time::Duration::seconds(lasts);
    let created = host
        .leader
        .post(
            &format!("/teams/{}/events", host.id),
            json!({ "name": name, "starts_at": starts_at.map(iso), "ends_at": iso(ends_at) }),
        )
        .await
        .ok();
    assert_eq!(created["synced"], true);
    EventFixture {
        id: created["id"].as_str().unwrap().to_string(),
        name,
        group: created["group_name"].as_str().unwrap().to_string(),
        starts_at: parse_time(&created["starts_at"]),
        ends_at: parse_time(&created["ends_at"]),
    }
}

pub async fn invite_to_event(host: &TeamFixture, event: &EventFixture, team: &TeamFixture) {
    host.leader.post(&format!("/teams/{}/events/{}/teams", host.id, event.id), json!({ "team_id": team.id })).await.ok();
}

/// The leader of an invited team answers; on acceptance the change must be live at once.
pub async fn answer_event(team: &TeamFixture, event: &EventFixture, accept: bool) {
    let answer =
        team.leader.post(&format!("/teams/{}/events/{}/answer", team.id, event.id), json!({ "accept": accept })).await.ok();
    assert_eq!(answer["synced"], true, "an answer must be applied when the call returns");
}

/// An event as `session` sees it from the team `team_id`; `Null` when it is not listed.
pub async fn event_seen_by(session: &Session, team_id: &str, event_id: &str) -> Value {
    let list = session.get(&format!("/teams/{team_id}/events")).await.ok();
    list["events"].as_array().unwrap().iter().find(|e| e["id"] == event_id).cloned().unwrap_or(Value::Null)
}

/// `name:status` of the teams listed in an event, sorted.
pub fn event_teams(event: &Value) -> Vec<String> {
    let mut teams: Vec<String> = event["teams"]
        .as_array()
        .unwrap_or_else(|| panic!("no event: {event}"))
        .iter()
        .map(|t| format!("{}:{}", t["name"].as_str().unwrap(), t["status"].as_str().unwrap()))
        .collect();
    teams.sort();
    teams
}

/// What the reconciliation journal holds about a group, oldest first.
pub async fn journal_of_group(admin: &Session, group: &str) -> Vec<Value> {
    let report = admin.get("/admin/reconciliation").await.ok();
    let mut entries: Vec<Value> =
        report["events"].as_array().unwrap().iter().filter(|e| e["group_name"] == group).cloned().collect();
    entries.reverse();
    entries
}

// ------------------------------------------------------------------------------- zips

pub struct Package {
    pub files: BTreeMap<String, Vec<u8>>,
}

impl Package {
    pub fn open(zip_bytes: &[u8]) -> Self {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes)).expect("a zip archive");
        let mut files = BTreeMap::new();
        for i in 0..archive.len() {
            let mut file = archive.by_index(i).unwrap();
            let mut data = Vec::new();
            file.read_to_end(&mut data).unwrap();
            files.insert(file.name().unwrap().to_string(), data);
        }
        Self { files }
    }

    pub fn names(&self) -> Vec<&str> {
        self.files.keys().map(String::as_str).collect()
    }

    pub fn pref(&self) -> String {
        String::from_utf8(self.files["config.pref"].clone()).unwrap()
    }

    /// Value of a `config.pref` entry.
    pub fn pref_value(&self, key: &str) -> Option<String> {
        let pref = self.pref();
        let start = pref.find(&format!("key=\"{key}\""))?;
        let rest = &pref[start..];
        let open = rest.find('>')? + 1;
        let close = rest.find("</entry>")?;
        Some(rest[open..close].to_string())
    }

    pub fn pref_keys(&self) -> Vec<String> {
        self.pref().split("key=\"").skip(1).map(|s| s[..s.find('"').unwrap()].to_string()).collect()
    }

    /// CA certificates of the truststore, opened with the password written in the zip.
    pub fn trust_roots(&self) -> Vec<CertificateDer<'static>> {
        let location = self.pref_value("caLocation0").expect("caLocation0");
        let file = location.strip_prefix("cert/").expect("stores are referenced under cert/");
        let password = self.pref_value("caPassword0").expect("caPassword0");
        let store = KeyStore::from_pkcs12(&self.files[file], &password, Pkcs12ImportPolicy::Relaxed).expect("truststore opens");
        store
            .entries()
            .filter_map(|(_, entry)| match entry {
                KeyStoreEntry::Certificate(c) => Some(CertificateDer::from(c.as_der().to_vec())),
                _ => None,
            })
            .collect()
    }

    /// Client certificate and key of a zip with certificate.
    pub fn identity(&self) -> Identity {
        let location = self.pref_value("certificateLocation0").expect("certificateLocation0");
        let file = location.strip_prefix("cert/").unwrap();
        let password = self.pref_value("clientPassword0").expect("clientPassword0");
        let store = KeyStore::from_pkcs12(&self.files[file], &password, Pkcs12ImportPolicy::Relaxed).expect("client store opens");
        let (_, chain) = store.private_key_chain().expect("a key and its certificate");
        Identity {
            chain: chain.certs().iter().map(|c| c.as_der().to_vec()).collect(),
            key: chain.key().as_der().to_vec(),
            roots: self.trust_roots(),
        }
    }
}

/// What a TAK client needs to connect: its certificate chain, its key, and the CAs it trusts.
#[derive(Clone)]
pub struct Identity {
    pub chain: Vec<Vec<u8>>,
    pub key: Vec<u8>,
    pub roots: Vec<CertificateDer<'static>>,
}

impl Identity {
    pub fn fingerprint(&self) -> String {
        teamtak_core::tak::fingerprint(&self.chain[0])
    }
}

fn root_store(roots: &[CertificateDer<'static>]) -> rustls::RootCertStore {
    let mut store = rustls::RootCertStore::empty();
    for root in roots {
        store.add(root.clone()).unwrap();
    }
    store
}

fn tls_builder() -> rustls::ConfigBuilder<rustls::ClientConfig, rustls::WantsVerifier> {
    rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
}

/// Enrolls a certificate the way a phone does from a username and a password: the name
/// entries from the server, a key and a signing request made locally, signed on port 8446.
/// `device` is the client UID the phone announces. Returns the HTTP status when the server
/// refuses.
pub async fn device_enroll(
    roots: &[CertificateDer<'static>],
    username: &str,
    password: &str,
    device: &str,
) -> Result<Identity, u16> {
    use teamtak_core::tak::enroll;
    let tls = tls_builder().with_root_certificates(root_store(roots)).with_no_client_auth();
    let http = reqwest::Client::builder()
        .tls_backend_preconfigured(tls)
        .resolve("localhost", "127.0.0.1:8446".parse().unwrap())
        .build()
        .unwrap();
    let base = format!("https://{TAK_SERVER_NAME}:8446");
    let config = http
        .get(format!("{base}/Marti/api/tls/config"))
        .basic_auth(username, Some(password))
        .send()
        .await
        .expect("8446 reachable");
    if !config.status().is_success() {
        return Err(config.status().as_u16());
    }
    let entries = enroll::parse_name_entries(&config.text().await.unwrap());
    let made = enroll::generate_key_and_csr(&entries, username).unwrap();
    let signed = http
        .post(format!("{base}/Marti/api/tls/signClient/v2?clientUid={device}&version=e2e"))
        .basic_auth(username, Some(password))
        .header("Content-Type", "text/plain")
        .header("Accept", "application/json")
        .body(enroll::csr_body(&made.csr_der))
        .send()
        .await
        .unwrap();
    if !signed.status().is_success() {
        return Err(signed.status().as_u16());
    }
    let body: Value = signed.json().await.unwrap();
    let enrolled = enroll::read_signed_response(username, made.private_key_der, &body).unwrap();
    let mut chain = vec![enrolled.certificate_der];
    chain.extend(enrolled.ca_ders);
    Ok(Identity { chain, key: enrolled.private_key_der, roots: roots.to_vec() })
}

/// Where TAK clients reach TAK Server's API: 8443 of the machine. `TM_TAK_API_ADDRESS`
/// (`ip:port`) points elsewhere, for instance at TAK Server's container itself.
pub fn tak_api_address() -> std::net::SocketAddr {
    std::env::var("TM_TAK_API_ADDRESS").unwrap_or_else(|_| "127.0.0.1:8443".to_string()).parse().expect("TM_TAK_API_ADDRESS: ip:port")
}

/// The groups TAK Server lists to a device, the way ATAK and iTAK read their channels: on its
/// API, 8443 of the server whatever port they stream on, with the device's certificate
/// (`GET /Marti/api/groups/all?useCache=true`). Each entry has `name`, `direction` (`IN`,
/// `OUT`) and `active`. An error says what answered instead.
pub async fn device_channels(identity: &Identity) -> Result<Vec<Value>, String> {
    install_crypto();
    let chain: Vec<CertificateDer<'static>> = identity.chain.iter().cloned().map(CertificateDer::from).collect();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(identity.key.clone()));
    let tls = tls_builder()
        .with_root_certificates(root_store(&identity.roots))
        .with_client_auth_cert(chain, key)
        .map_err(|e| e.to_string())?;
    let address = tak_api_address();
    let http = reqwest::Client::builder()
        .tls_backend_preconfigured(tls)
        .resolve(TAK_SERVER_NAME, address)
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())?;
    let url = format!("https://{TAK_SERVER_NAME}:{}/Marti/api/groups/all?useCache=true", address.port());
    let answer = http.get(&url).send().await.map_err(|e| format!("{url} ({address}): {e:?}"))?;
    let status = answer.status();
    let body = answer.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!("{url} ({address}): HTTP {status}: {body}"));
    }
    let value: Value = serde_json::from_str(&body).map_err(|e| format!("{url}: not JSON ({e}): {body}"))?;
    value["data"].as_array().cloned().ok_or_else(|| format!("{url}: no data: {body}"))
}

// ------------------------------------------------------------------------------- TAK client

/// A scripted TAK client: mutual TLS on the main client port (8089 by default), one position
/// (CoT XML) per second (or per
/// `period`), every received event recorded. It never reconnects: a client that still
/// receives after a change kept the same connection through it. It does not negotiate the protobuf protocol, so the server
/// keeps talking XML, as it does with ATAK until negotiation.
pub struct TakClient {
    pub uid: String,
    seen: Arc<Mutex<Vec<(Instant, String)>>>,
    closed: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
}

fn cot_time(t: time::OffsetDateTime) -> String {
    let format = time::macros::format_description!("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z");
    t.format(&format).unwrap()
}

fn position_event(uid: &str) -> String {
    let now = time::OffsetDateTime::now_utc();
    format!(
        "<event version=\"2.0\" uid=\"{uid}\" type=\"a-f-G-U-C\" how=\"m-g\" time=\"{t}\" start=\"{t}\" stale=\"{s}\">\
         <point lat=\"45.9\" lon=\"6.1\" hae=\"450\" ce=\"10\" le=\"10\"/>\
         <detail><contact callsign=\"{uid}\" endpoint=\"*:-1:stcp\"/>\
         <takv device=\"e2e\" platform=\"teamtak-e2e\" os=\"0\" version=\"0\"/></detail></event>",
        t = cot_time(now),
        s = cot_time(now + time::Duration::seconds(30)),
    )
}

impl TakClient {
    /// Opens a connection; an error means the server refused the handshake.
    pub async fn connect(identity: &Identity, label: &str) -> Result<Self, String> {
        Self::connect_every(identity, label, Duration::from_secs(1)).await
    }

    /// Same, sending a position every `period`: a shorter one measures delays more finely.
    pub async fn connect_every(identity: &Identity, label: &str, period: Duration) -> Result<Self, String> {
        Self::open(identity, label, period, client_ports()[0]).await
    }

    /// Opens a connection on another of the client ports.
    pub async fn connect_on(identity: &Identity, label: &str, port: u16) -> Result<Self, String> {
        Self::open(identity, label, Duration::from_secs(1), port).await
    }

    async fn open(identity: &Identity, label: &str, period: Duration, port: u16) -> Result<Self, String> {
        install_crypto();
        let chain: Vec<CertificateDer<'static>> = identity.chain.iter().cloned().map(CertificateDer::from).collect();
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(identity.key.clone()));
        let tls = tls_builder()
            .with_root_certificates(root_store(&identity.roots))
            .with_client_auth_cert(chain, key)
            .map_err(|e| e.to_string())?;
        let connector = tokio_rustls::TlsConnector::from(Arc::new(tls));
        let tcp = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.map_err(|e| e.to_string())?;
        let name = ServerName::try_from(TAK_SERVER_NAME).unwrap();
        let stream = tokio::time::timeout(Duration::from_secs(10), connector.connect(name, tcp))
            .await
            .map_err(|_| "TLS handshake timed out".to_string())?
            .map_err(|e| format!("TLS handshake: {e}"))?;

        let uid = unique(&format!("E2E-{label}-"));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let closed = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let (mut reader, mut writer) = tokio::io::split(stream);

        let (uid_w, closed_w, stop_w) = (uid.clone(), closed.clone(), stop.clone());
        tokio::spawn(async move {
            while !stop_w.load(Ordering::SeqCst) && !closed_w.load(Ordering::SeqCst) {
                if writer.write_all(position_event(&uid_w).as_bytes()).await.is_err() || writer.flush().await.is_err() {
                    closed_w.store(true, Ordering::SeqCst);
                    break;
                }
                tokio::time::sleep(period).await;
            }
            let _ = writer.shutdown().await;
        });

        let (seen_r, closed_r) = (seen.clone(), closed.clone());
        tokio::spawn(async move {
            let mut pending = String::new();
            let mut buffer = vec![0u8; 16 * 1024];
            loop {
                match reader.read(&mut buffer).await {
                    Ok(0) | Err(_) => {
                        closed_r.store(true, Ordering::SeqCst);
                        break;
                    }
                    Ok(n) => {
                        pending.push_str(&String::from_utf8_lossy(&buffer[..n]));
                        while let Some(end) = pending.find("</event>") {
                            let event: String = pending.drain(..end + "</event>".len()).collect();
                            if let Some(start) = event.find("<event") {
                                if let Some(uid) = attribute(&event[start..], "uid") {
                                    seen_r.lock().unwrap().push((Instant::now(), uid));
                                }
                            }
                        }
                    }
                }
            }
        });
        Ok(Self { uid, seen, closed, stop })
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// True when a position of `other` arrived after `since`.
    pub fn saw_since(&self, other: &TakClient, since: Instant) -> bool {
        self.seen.lock().unwrap().iter().any(|(at, uid)| *at >= since && uid == &other.uid)
    }

    /// When the first position of `other` arrived at or after `since`.
    pub fn first_seen_since(&self, other: &TakClient, since: Instant) -> Option<Instant> {
        self.seen.lock().unwrap().iter().find(|(at, uid)| *at >= since && uid == &other.uid).map(|(at, _)| *at)
    }

    /// When the latest position of `other` arrived.
    pub fn last_seen(&self, other: &TakClient) -> Option<Instant> {
        self.seen.lock().unwrap().iter().rev().find(|(_, uid)| uid == &other.uid).map(|(at, _)| *at)
    }

    /// Waits until a position of `other` arrives at or after `since`, and returns when it did.
    pub async fn wait_first_seen(&self, other: &TakClient, since: Instant, timeout: Duration) -> Option<Instant> {
        let deadline = Instant::now().max(since) + timeout;
        loop {
            if let Some(at) = self.first_seen_since(other, since) {
                return Some(at);
            }
            if Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Waits until a position of `other` arrives; false after `timeout`.
    pub async fn sees(&self, other: &TakClient, timeout: Duration) -> bool {
        let since = Instant::now();
        let deadline = since + timeout;
        while Instant::now() < deadline {
            if self.saw_since(other, since) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        false
    }

    /// Waits until the server closes the connection; returns how long it took.
    pub async fn closed_within(&self, timeout: Duration) -> Option<Duration> {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if self.is_closed() {
                return Some(start.elapsed());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        None
    }

    pub fn close(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

impl Drop for TakClient {
    fn drop(&mut self) {
        self.close();
    }
}

fn attribute(tag: &str, name: &str) -> Option<String> {
    let end = tag.find('>')?;
    let head = &tag[..end];
    let start = head.find(&format!(" {name}=\""))? + name.len() + 3;
    let stop = head[start..].find('"')? + start;
    Some(head[start..stop].to_string())
}

/// True when an identity can no longer be used: the handshake fails, or the server closes
/// the connection within a few seconds.
pub async fn is_rejected(identity: &Identity) -> bool {
    match TakClient::connect(identity, "revoked").await {
        Err(_) => true,
        Ok(client) => client.closed_within(Duration::from_secs(8)).await.is_some(),
    }
}
