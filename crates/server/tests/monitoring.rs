//! Integration tests of phase 3 against the real stack: the host agent, alerts by e-mail,
//! connected devices, the audit log, backups and the actions that go through the agent.
//!
//! Like the other suites they need the stack in local mode (`./install.sh --local`) and
//! are ignored by a plain `cargo test`; run them with `scripts/test-integration.sh`.
//!
//! Local mode looks at everything every 10 seconds (`MONITOR_INTERVAL_SECONDS`), and an
//! alert is raised or resolved after two looks in a row: the waits below follow from that.
//! Two tests stop or restart a service of the stack (the reverse proxy, the mail catcher)
//! and put it back.

mod common;

use std::time::{Duration, Instant};

use common::*;
use serde_json::{json, Value};

/// Long enough for two looks of the monitoring and the e-mails that follow.
const ALERT_WAIT: Duration = Duration::from_secs(90);

/// Runs `teamtak-agent call <method> <path>` inside the agent's container: the only
/// place its API can be reached from, besides the platform.
fn agent_call(method: &str, path: &str, token: Option<&str>) -> (u16, Value) {
    let mut args: Vec<String> = vec!["compose".into(), "exec".into(), "-T".into()];
    if let Some(token) = token {
        args.extend(["-e".to_string(), format!("AGENT_TOKEN={token}")]);
    }
    args.extend(["agent", "teamtak-agent", "call", method, path].map(String::from));
    let output = std::process::Command::new("docker").current_dir(repo_root().join("deploy")).args(&args).output().expect("docker compose exec");
    let text = String::from_utf8_lossy(&output.stdout);
    let mut lines = text.lines();
    let status: u16 = lines.next().and_then(|l| l.trim().parse().ok()).unwrap_or_else(|| panic!("no status in: {text}{}", String::from_utf8_lossy(&output.stderr)));
    let body = lines.collect::<Vec<_>>().join("\n");
    (status, serde_json::from_str(&body).unwrap_or(Value::Null))
}

/// The monitoring as an administrator sees it, after a fresh look.
async fn fresh_look(admin: &Session) -> Value {
    admin.get("/admin/monitoring?refresh=true").await.ok()
}

fn services(monitoring: &Value) -> Vec<&Value> {
    monitoring["snapshot"]["services"]["services"].as_array().expect("services").iter().collect()
}

fn open_alerts(monitoring: &Value) -> Vec<(String, String)> {
    monitoring["alerts"]["open"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| (a["kind"].as_str().unwrap().to_string(), a["subject"].as_str().unwrap().to_string()))
        .collect()
}

/// Every message received by `to`, newest first.
async fn mails_to(to: &str) -> Vec<Mail> {
    let http = reqwest::Client::new();
    let list: Value = http
        .get(format!("{}/api/v1/search", mailpit_url()))
        .query(&[("query", format!("to:{to}")), ("limit", "200".to_string())])
        .send()
        .await
        .expect("mail catcher reachable")
        .json()
        .await
        .unwrap();
    let mut mails = Vec::new();
    for message in list["messages"].as_array().cloned().unwrap_or_default() {
        let id = message["ID"].as_str().unwrap();
        let full: Value = http.get(format!("{}/api/v1/message/{id}", mailpit_url())).send().await.unwrap().json().await.unwrap();
        // E-mail travels with CRLF line ends.
        let text = full["Text"].as_str().unwrap_or_default().replace("\r\n", "\n");
        mails.push(Mail { subject: full["Subject"].as_str().unwrap_or_default().to_string(), text });
    }
    mails
}

/// Waits for a message to `to` whose text contains `needle`.
async fn mail_containing(to: &str, needle: &str, timeout: Duration) -> Mail {
    mail_where(to, &format!("containing {needle:?}"), |text| text.contains(needle), timeout).await
}

/// The first e-mail to `to` whose text passes `test`, waiting for it up to `timeout`.
async fn mail_where(to: &str, what: &str, test: impl Fn(&str) -> bool, timeout: Duration) -> Mail {
    let deadline = Instant::now() + timeout;
    loop {
        let mails = mails_to(to).await;
        if let Some(found) = mails.iter().find(|m| test(&m.text)) {
            return Mail { subject: found.subject.clone(), text: found.text.clone() };
        }
        let subjects: Vec<&str> = mails.iter().map(|m| m.subject.as_str()).collect();
        assert!(Instant::now() < deadline, "no e-mail {what} received by {to} in {timeout:?}; it has: {subjects:?}");
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// Whether `text` lists `sentence` as resolved: "  - [resolved after 41 seconds] <sentence>".
fn lists_as_resolved(text: &str, tag: &str, sentence: &str) -> bool {
    text.lines().any(|line| line.starts_with(&format!("  - [{tag} ")) && line.ends_with(&format!("] {sentence}")))
}

/// Polls the monitoring until `ready` says so.
async fn monitoring_until(admin: &Session, what: &str, timeout: Duration, ready: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + timeout;
    loop {
        let monitoring = admin.get("/admin/monitoring").await.ok();
        if ready(&monitoring) {
            return monitoring;
        }
        assert!(Instant::now() < deadline, "{what}: not seen in {timeout:?}; open alerts: {:?}", open_alerts(&monitoring));
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// The audit log with a filter, newest first.
async fn audit(admin: &Session, query: &str) -> Value {
    admin.get(&format!("/admin/audit?page_size=200&{query}")).await.ok()
}

fn actions(log: &Value) -> Vec<&str> {
    log["entries"].as_array().unwrap().iter().map(|e| e["action"].as_str().unwrap()).collect()
}

/// Starts a stopped service again when the test ends, whatever its outcome.
struct StartAgain(&'static str);

impl Drop for StartAgain {
    fn drop(&mut self) {
        let _ = compose(&["start", self.0]);
    }
}

// ------------------------------------------------------------------------------ the agent

#[tokio::test]
#[ignore = "needs the running stack"]
async fn the_agent_reports_the_machine_and_this_stack_only() {
    let admin = Session::admin().await;
    let monitoring = fresh_look(&admin).await;
    let snapshot = &monitoring["snapshot"];
    assert_eq!(snapshot["agent"]["status"], "ok", "{}", snapshot["agent"]);
    assert_eq!(snapshot["mode"], "local");

    // The machine: figures a host really has, not placeholders.
    let host = &snapshot["host"];
    assert!(host["cpu"]["cores"].as_u64().unwrap() >= 1);
    assert!(host["cpu"]["load1"].is_number() && host["uptime_seconds"].as_u64().unwrap() > 0);
    let memory = &host["memory"];
    assert!(memory["total_bytes"].as_u64().unwrap() > memory["available_bytes"].as_u64().unwrap());
    let disks = host["disks"].as_array().unwrap();
    assert!(disks.iter().any(|d| d["name"] == "docker" || d["also"].as_array().unwrap().contains(&json!("docker"))), "Docker's disk is reported: {disks:?}");
    for disk in disks {
        let used = disk["used_percent"].as_f64().unwrap();
        assert!((0.0..=100.0).contains(&used) && disk["total_bytes"].as_u64().unwrap() > 0, "{disk}");
    }
    assert!(host["clock"]["synchronized"].is_boolean(), "the kernel's view of the clock is read: {}", host["clock"]);
    // The development machine's Docker VM is not Debian or Ubuntu: unknown, not "up to date".
    if host["os"]["id"] != "debian" && host["os"]["id"] != "ubuntu" {
        assert!(host["updates"]["manager"].is_null() && host["updates"]["pending"].is_null(), "{}", host["updates"]);
    }

    // The services of this stack, all of them, and nothing of the other projects that
    // share this Docker.
    let listed = services(&monitoring);
    let names: Vec<&str> = listed.iter().map(|s| s["service"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["caddy", "app", "postgres", "openldap", "takserver", "takserver-db", "agent", "mailpit"]);
    for service in &listed {
        assert_eq!((service["present"].as_bool(), service["state"].as_str()), (Some(true), Some("running")), "{service}");
    }
    let image_status = |name: &str| listed.iter().find(|s| s["service"] == name).unwrap()["update"]["source"].as_str().unwrap().to_string();
    assert_eq!(image_status("app"), "local");
    assert_eq!(image_status("takserver"), "local");
    assert_eq!(image_status("caddy"), "tag");
    assert_eq!(image_status("openldap"), "digest");
    assert!(listed.iter().find(|s| s["service"] == "agent").is_some_and(|s| s["restartable"] == false), "the agent does not offer to restart itself");

    // TAK Server: its three ports, its version, its five processes.
    let tak = &snapshot["tak"];
    let ports: Vec<(u64, bool)> = tak["ports"].as_array().unwrap().iter().map(|p| (p["port"].as_u64().unwrap(), p["open"].as_bool().unwrap())).collect();
    assert_eq!(ports, vec![(8089, true), (8443, true), (8446, true)]);
    assert_eq!(tak["api_ok"], true);
    assert!(tak["version"].as_str().unwrap().starts_with(setting("TAK_VERSION")), "{}", tak["version"]);
    assert_eq!(tak["installed_version"], setting("TAK_VERSION"));
    let processes: Vec<(&str, bool)> = tak["processes"].as_array().unwrap().iter().map(|p| (p["name"].as_str().unwrap(), p["running"].as_bool().unwrap())).collect();
    assert_eq!(processes, vec![("config", true), ("messaging", true), ("api", true), ("retention", true), ("plugins", true)]);
    assert!(tak["other_zip_versions"].as_array().unwrap().is_empty(), "the only ZIP at the root is the installed version");

    // Certificates: the authority and the platform's own from its files, TAK Server's as it
    // presents it. The intermediate authority was made for ten years, the others for two.
    let certificate = |id: &str| snapshot["certificates"].as_array().unwrap().iter().find(|c| c["id"] == id).cloned().unwrap_or_else(|| panic!("no certificate {id}: {}", snapshot["certificates"]));
    assert!(certificate("root_ca")["days_left"].as_i64().unwrap() > 3000);
    assert!(certificate("intermediate_ca")["days_left"].as_i64().unwrap() > 3000);
    let server = certificate("tak_server");
    assert_eq!((server["subject"].as_str(), server["from"].as_str(), server["level"].as_str()), (Some("takserver"), Some("presented"), Some("ok")));
    assert!((600..=730).contains(&server["days_left"].as_i64().unwrap()), "{server}");
    assert_eq!(certificate("platform_api")["subject"], setting("TAK_API_CERT_NAME"));

    // The platform itself.
    let platform = &snapshot["platform"];
    assert_eq!((platform["directory_open"].as_bool(), platform["reconcile"]["tak_reachable"].as_bool()), (Some(true), Some(true)));
    assert!(platform["reconcile"]["desired_at"].is_string() && platform["overdue_events"].as_array().unwrap().is_empty());
    assert_eq!(platform["mail"]["configured"], true);

    // The same report straight from the agent, and what it refuses.
    let (status, report) = agent_call("GET", "/v1/report", None);
    assert_eq!(status, 200);
    let containers = report["containers"]["containers"].as_array().unwrap();
    assert_eq!(containers.len(), 8);
    assert!(containers.iter().all(|c| c["name"].as_str().unwrap().starts_with("teamtak-")), "only this project's containers: {containers:?}");
    let text = report.to_string();
    for secret in ["APP_SECRET", "AGENT_TOKEN", "POSTGRES_PASSWORD", "TAK_CERT_PASSWORD"] {
        assert!(!text.contains(setting(secret)), "the report must not carry {secret}");
    }

    // Other projects run on this Docker. Whatever their containers are called, the agent
    // refuses, and it refuses itself, container names, identifiers and anything unknown.
    let others = std::process::Command::new("docker").args(["ps", "-a", "--format", "{{.Names}}"]).output().unwrap();
    let others = String::from_utf8_lossy(&others.stdout);
    let foreign: Vec<&str> = others.lines().filter(|n| !n.starts_with("teamtak-") && n.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))).take(5).collect();
    for target in foreign.iter().copied().chain(["agent", "teamtak-caddy-1", "no-such-service", "caddy;id"]) {
        let (status, body) = agent_call("POST", &format!("/v1/services/{target}/restart"), None);
        assert_eq!((status, body["error"].as_str()), (403, Some("service_not_allowed")), "{target}");
    }
    assert_eq!(agent_call("POST", "/v1/exec", None).0, 404, "there is no way to run a command");
    assert_eq!(agent_call("GET", "/v1/report", Some("not-the-secret")).0, 401);
    // And the platform refuses the same before even asking the agent.
    for target in ["agent", "mapping-db-1", "teamtak-caddy-1"] {
        admin.post(&format!("/admin/services/{target}/restart"), json!({})).await.expect_error(400, "service_not_allowed");
    }

    // Monitoring is for administrators.
    let leader = Session::new_account("nomon").await;
    for path in ["/admin/monitoring", "/admin/monitoring/history", "/admin/devices", "/admin/audit", "/admin/settings", "/admin/alerts/summary"] {
        leader.get(path).await.expect_error(403, "forbidden");
        Session::anonymous().get(path).await.expect_error(401, "unauthenticated");
    }
    leader.post("/admin/backups/run", json!({})).await.expect_error(403, "forbidden");
    leader.post("/admin/services/mailpit/restart", json!({})).await.expect_error(403, "forbidden");
}

// ------------------------------------------------------------------------------ alerts

#[tokio::test]
#[ignore = "needs the running stack"]
async fn a_stopped_service_raises_an_alert_by_email_and_its_resolution() {
    // Two administrators, one who reads French and one who reads English.
    let french = Session::admin().await;
    french.patch("/me", json!({ "language": "fr" })).await.ok();
    let english = Session::admin().await;
    // Everything is up, and whatever else is open on this machine was announced before
    // these two accounts existed.
    let before = fresh_look(&french).await;
    assert!(!open_alerts(&before).contains(&("service_down".to_string(), "caddy".to_string())));

    let down_fr = "Le service caddy ne tourne pas (état : exited).";
    let down_en = "The service caddy is not running (state: exited).";
    let stopped_at = Instant::now();
    {
        let (ok, output) = compose(&["stop", "caddy"]);
        let _restart = StartAgain("caddy");
        assert!(ok, "stopping the reverse proxy failed: {output}");

        // One e-mail, in each administrator's language.
        let mail = mail_containing(&french.email, down_fr, ALERT_WAIT).await;
        println!("alert e-mail received {:.0} s after the service stopped: {}", stopped_at.elapsed().as_secs_f32(), mail.subject);
        assert!(mail.subject.starts_with("[TeamTAK] Alerte"), "{}", mail.subject);
        assert!(mail.text.contains(&format!("Nouvelles alertes :\n  - [critique] {down_fr}")), "{}", mail.text);
        assert!(mail.text.contains("http://localhost:8480/monitoring"), "the e-mail links to the monitoring page: {}", mail.text);
        let mail = mail_containing(&english.email, down_en, Duration::from_secs(20)).await;
        assert!(mail.subject.starts_with("[TeamTAK] Alert"), "{}", mail.subject);
        assert!(mail.text.contains(&format!("New alerts:\n  - [critical] {down_en}")), "{}", mail.text);

        // While it lasts nothing more is sent: one notification, not one per look.
        tokio::time::sleep(Duration::from_secs(25)).await;
        let about_caddy = mails_to(&french.email).await.iter().filter(|m| m.text.contains(down_fr)).count();
        assert_eq!(about_caddy, 1, "an open alert is not announced again at every look");
    }

    // Back up (the guard started it): the alert resolves, and says so once.
    start_platform().await;
    let resolved = mail_where(&french.email, "saying caddy is back", |text| lists_as_resolved(text, "résolue après", down_fr), ALERT_WAIT).await;
    assert!(resolved.text.contains("Résolues (le problème a disparu) :\n"), "{}", resolved.text);
    assert!(resolved.subject.starts_with("[TeamTAK] Résolu") || resolved.subject.starts_with("[TeamTAK] Alertes"), "{}", resolved.subject);
    let resolved = mail_where(&english.email, "saying caddy is back", |text| lists_as_resolved(text, "resolved after", down_en), Duration::from_secs(20)).await;
    assert!(resolved.text.contains("Resolved (the problem is gone):\n"), "{}", resolved.text);

    let after = fresh_look(&french).await;
    assert!(!open_alerts(&after).contains(&("service_down".to_string(), "caddy".to_string())));
    let closed = after["alerts"]["resolved"].as_array().unwrap().iter().find(|a| a["kind"] == "service_down" && a["subject"] == "caddy").cloned().expect("the alert is in the recent history");
    assert_eq!((closed["severity"].as_str(), closed["details"]["state"].as_str()), (Some("critical"), Some("exited")));
    assert!(closed["resolved_at"].is_string());
    let summary = french.get("/admin/alerts/summary").await.ok();
    assert_eq!(summary["open"].as_u64().unwrap() as usize, open_alerts(&after).len());
}

#[tokio::test]
#[ignore = "needs the running stack"]
async fn thresholds_are_settings_and_a_change_raises_and_resolves_an_alert() {
    let admin = Session::admin().await;
    let view = admin.get("/admin/settings").await.ok();
    assert_eq!(view["defaults"]["thresholds"]["disk_percent"], 90.0);
    assert_eq!((view["defaults"]["backup_hour_utc"].as_u64(), view["defaults"]["audit_retention_days"].as_u64()), (Some(3), Some(365)));

    // What is refused, and what is brought within bounds.
    for bad in [
        json!({ "settings": { "no_such_setting": 1 } }),
        json!({ "settings": { "webhook_url": "ftp://example.org" } }),
        json!({ "settings": { "backup_hour_utc": 24 } }),
        json!({ "settings": { "tak_latest_known_version": "newest" } }),
        json!({ "backup_passphrase": "short" }),
    ] {
        admin.patch("/admin/settings", bad.clone()).await.expect_error(400, "invalid_input");
    }
    admin.post("/admin/settings/webhook-test", json!({})).await.expect_error(409, "webhook_not_set");

    // Docker's disk on this machine is somewhere between half full and 90 %: a lower
    // threshold makes it an alert, the default one resolves it.
    let monitoring = fresh_look(&admin).await;
    let fullest = monitoring["snapshot"]["host"]["disks"].as_array().unwrap().iter().filter(|d| d["used_percent"].as_f64().unwrap() < 90.0).max_by(|a, b| a["used_percent"].as_f64().partial_cmp(&b["used_percent"].as_f64()).unwrap()).cloned();
    let Some(disk) = fullest.filter(|d| d["used_percent"].as_f64().unwrap() >= 50.0) else {
        println!("no filesystem between 50 % and 90 % full on this machine: the threshold part is skipped");
        return;
    };
    let name = disk["name"].as_str().unwrap().to_string();
    let key = ("disk_full".to_string(), name.clone());
    assert!(!open_alerts(&monitoring).contains(&key), "{name} is under the default threshold");

    let saved = admin.patch("/admin/settings", json!({ "settings": { "thresholds": { "disk_percent": 50 } } })).await.ok();
    assert_eq!(saved["settings"]["thresholds"]["disk_percent"], 50.0);
    assert_eq!(saved["settings"]["thresholds"]["memory_percent"], 92.0, "the other thresholds keep their value");
    let raised = monitoring_until(&admin, "disk alert with a threshold of 50 %", ALERT_WAIT, |m| open_alerts(m).contains(&key)).await;
    let alert = raised["alerts"]["open"].as_array().unwrap().iter().find(|a| a["kind"] == "disk_full" && a["subject"] == name.as_str()).unwrap();
    assert!((alert["details"]["percent"].as_f64().unwrap() - disk["used_percent"].as_f64().unwrap()).abs() < 5.0, "{alert}");
    let mail = mail_containing(&admin.email, "Disk nearly full: ", Duration::from_secs(30)).await;
    assert!(mail.subject.starts_with("[TeamTAK] Alert"), "{}", mail.subject);

    admin.patch("/admin/settings", json!({ "settings": { "thresholds": { "disk_percent": 90 } } })).await.ok();
    monitoring_until(&admin, "disk alert resolved with the default threshold", ALERT_WAIT, |m| !open_alerts(m).contains(&key)).await;

    // The change is in the audit log, by name; and a history sample exists for the charts.
    let log = audit(&admin, "action=settings_changed").await;
    let mine = log["entries"].as_array().unwrap().iter().find(|e| e["actor_email"] == admin.email.as_str()).expect("the change is recorded");
    assert_eq!(mine["details"]["settings"], "thresholds.disk_percent");
    let history = admin.get("/admin/monitoring/history?hours=24").await.ok();
    let samples = history["samples"].as_array().unwrap();
    assert!(!samples.is_empty() && samples.len() <= 300, "{} samples", samples.len());
    assert!(samples.last().unwrap()["memory_percent"].is_number() && samples.last().unwrap()["devices"].is_number(), "{}", samples.last().unwrap());
}

#[tokio::test]
#[ignore = "needs the running stack"]
async fn leaders_are_told_in_their_language_when_a_zip_certificate_nears_its_end() {
    let admin = Session::admin().await;
    let team = approved_team(&admin, "shared", 5).await;
    team.leader.patch("/me", json!({ "language": "fr" })).await.ok();
    let member = add_member(&team).await;
    // A zip with certificate: valid 365 days, so about 364 whole days are left.
    let access = create_access(&team.leader, &team.id, json!({ "kind": "shared" })).await;
    team.leader.package(&access, "certificate").await;
    let username = team.leader.get(&format!("/accesses/{access}")).await.ok()["username"].as_str().unwrap().to_string();

    // Nobody is told a year ahead...
    tokio::time::sleep(Duration::from_secs(12)).await;
    assert!(!mail_subjects(&team.leader.email).await.iter().any(|s| s.contains("certificat TAK")));

    // ...until the administrator's thresholds say it is close. The longest warning period
    // the settings accept is a year: every zip certificate of this test stack is then "close".
    admin.patch("/admin/settings", json!({ "settings": { "thresholds": { "certificate_warning_days": 365 } } })).await.ok();
    let mail = mail_containing(&team.leader.email, &username, Duration::from_secs(60)).await;
    admin.patch("/admin/settings", json!({ "settings": { "thresholds": { "certificate_warning_days": 30 } } })).await.ok();
    assert!(mail.subject.starts_with(&format!("{} : le certificat TAK de {username} expire dans 36", team.name)), "{}", mail.subject);
    assert!(mail.text.contains("Régénérer") && mail.text.contains("Rien ne le renouvelle") && mail.text.contains(&format!("/teams/{}", team.id)), "{}", mail.text);
    assert!(mail.text.contains(" UTC"), "the date says its time zone: {}", mail.text);

    // Once, and to leaders only.
    tokio::time::sleep(Duration::from_secs(12)).await;
    let notices = mail_subjects(&team.leader.email).await.iter().filter(|s| s.contains("certificat TAK")).count();
    assert_eq!(notices, 1, "one notice per certificate and threshold");
    assert!(!mail_subjects(&member.email).await.iter().any(|s| s.contains("certificate of") || s.contains("certificat TAK")), "members are not written to");

    // Administrators see client certificates as one line of the dashboard, not one alert each.
    let monitoring = fresh_look(&admin).await;
    let clients = &monitoring["snapshot"]["client_certificates"];
    assert!(clients["active"].as_i64().unwrap() >= 1 && clients["soonest"].as_array().unwrap().len() <= 20);
    assert!(clients["soonest"].as_array().unwrap().iter().all(|c| c["username"].is_string() && c["team_name"].is_string() && c["days_left"].is_i64()));
}

// ------------------------------------------------------------------------------ devices

#[tokio::test]
#[ignore = "needs the running stack"]
async fn connected_devices_are_seen_by_administrators_and_by_the_leaders_of_the_team() {
    let admin = Session::admin().await;
    // A team on a shared identity with two devices, and another team with one.
    let alpha = approved_team(&admin, "shared", 5).await;
    let member = add_member(&alpha).await;
    let shared = access_identity(&alpha.leader, &alpha.id, json!({ "kind": "shared" })).await;
    let bravo = approved_team(&admin, "shared", 5).await;
    let other = access_identity(&bravo.leader, &bravo.id, json!({ "kind": "shared" })).await;
    let phone = TakClient::connect(&shared, "phone").await.expect("first device connects");
    let tablet = TakClient::connect(&shared, "tablet").await.expect("second device on the same identity connects");
    let stranger = TakClient::connect(&other, "other").await.expect("the other team's device connects");
    assert!(phone.sees(&tablet, Duration::from_secs(12)).await, "both devices are live on TAK Server");

    // The administrator sees every device, with its team, its access and where it connects from.
    let deadline = Instant::now() + Duration::from_secs(45);
    let all = loop {
        let list = admin.get("/admin/devices").await.ok();
        let callsigns: Vec<&str> = list["devices"].as_array().unwrap().iter().filter_map(|d| d["callsign"].as_str()).collect();
        if [&phone.uid, &tablet.uid, &stranger.uid].iter().all(|uid| callsigns.contains(&uid.as_str())) {
            break list;
        }
        assert!(Instant::now() < deadline, "the devices did not appear; listed: {callsigns:?}");
        tokio::time::sleep(Duration::from_secs(1)).await;
    };
    assert!(all["as_of"].is_string());
    let device = |list: &Value, uid: &str| list["devices"].as_array().unwrap().iter().find(|d| d["callsign"] == uid).cloned().unwrap_or_else(|| panic!("{uid} not listed"));
    let seen = device(&all, &phone.uid);
    assert_eq!(seen["username"], format!("{}-shared", alpha.slug));
    assert_eq!((seen["access"]["kind"].as_str(), seen["access"]["team_name"].as_str()), (Some("shared"), Some(alpha.name.as_str())));
    assert_eq!(seen["groups"], json!([alpha.slug]), "the channels of the session");
    assert!(seen["connected_since"].is_string() && seen["ip_address"].is_string(), "{seen}");
    assert_eq!(seen["silent"], false);
    assert_eq!(device(&all, &stranger.uid)["access"]["team_name"], bravo.name.as_str());

    // A leader sees the devices on their own team's accesses: two on the shared identity,
    // which nothing else can tell them. Not the other team's, and no address.
    let mine = alpha.leader.get(&format!("/teams/{}/devices", alpha.id)).await.ok();
    let mut callsigns: Vec<&str> = mine["devices"].as_array().unwrap().iter().filter_map(|d| d["callsign"].as_str()).collect();
    callsigns.sort();
    let mut expected = vec![phone.uid.as_str(), tablet.uid.as_str()];
    expected.sort();
    assert_eq!(callsigns, expected);
    for d in mine["devices"].as_array().unwrap() {
        assert!(d.get("ip_address").is_none(), "a leader is not given addresses: {d}");
        assert_eq!(d["username"], format!("{}-shared", alpha.slug));
    }
    assert!(mine.get("unidentified").is_none());
    assert_eq!(alpha.leader.get(&format!("/teams/{}", alpha.id)).await.ok()["permissions"]["view_devices"], true);

    // Members do not; a leader of another team does not even see the team; nobody but an
    // administrator gets the whole list.
    member.get(&format!("/teams/{}/devices", alpha.id)).await.expect_error(403, "forbidden");
    assert_eq!(member.get(&format!("/teams/{}", alpha.id)).await.ok()["permissions"]["view_devices"], false);
    bravo.leader.get(&format!("/teams/{}/devices", alpha.id)).await.expect_error(404, "not_found");
    alpha.leader.get("/admin/devices").await.expect_error(403, "forbidden");
    // The administrator, who is not a member, also has the team's list on its page.
    let as_admin = admin.get(&format!("/teams/{}/devices", alpha.id)).await.ok();
    assert_eq!(as_admin["devices"].as_array().unwrap().len(), 2);

    // The dashboard counts them, and a device that leaves leaves the list.
    let monitoring = fresh_look(&admin).await;
    assert!(monitoring["snapshot"]["devices"]["identified"].as_u64().unwrap() >= 3);
    tablet.close();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let mine = alpha.leader.get(&format!("/teams/{}/devices", alpha.id)).await.ok();
        let left: Vec<&str> = mine["devices"].as_array().unwrap().iter().filter_map(|d| d["callsign"].as_str()).collect();
        if left == vec![phone.uid.as_str()] {
            break;
        }
        assert!(Instant::now() < deadline, "the closed device is still listed: {left:?}");
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

// ------------------------------------------------------------------------------ audit log

#[tokio::test]
#[ignore = "needs the running stack"]
async fn the_audit_log_records_who_did_what_and_never_a_secret() {
    let admin = Session::admin().await;
    // A representative path through the platform.
    let team = approved_team(&admin, "shared", 6).await;
    let member = add_member(&team).await;
    let member_id = member.account_id().await;
    let access = create_access(&team.leader, &team.id, json!({ "kind": "shared" })).await;
    let detail = team.leader.get(&format!("/accesses/{access}")).await.ok();
    let password = detail["password"].as_str().unwrap().to_string();
    let username = detail["username"].as_str().unwrap().to_string();
    let zip = Package::open(&team.leader.package(&access, "certificate").await);
    let store_password = zip.pref_value("clientPassword0").unwrap();
    team.leader.package(&access, "enrollment").await;
    team.leader.post(&format!("/accesses/{access}/regenerate"), json!({})).await.ok();
    let new_password = team.leader.get(&format!("/accesses/{access}")).await.ok()["password"].as_str().unwrap().to_string();
    team.leader.patch(&format!("/teams/{}/members/{member_id}", team.id), json!({ "role": "leader" })).await.ok();
    let event = create_event(&team, Some(3600), 3600).await;
    team.leader.post(&format!("/teams/{}/events/{}/cancel", team.id, event.id), json!({})).await.ok();
    admin.patch(&format!("/admin/teams/{}", team.id), json!({ "member_cap": 9 })).await.ok();
    admin.patch(&format!("/admin/teams/{}", team.id), json!({ "suspended": true })).await.ok();
    admin.patch(&format!("/admin/teams/{}", team.id), json!({ "suspended": false })).await.ok();
    admin.post("/admin/reconciliation/run", json!({})).await.ok();
    team.leader.delete(&format!("/accesses/{access}")).await.ok();
    team.leader.delete(&format!("/teams/{}", team.id)).await.ok();

    // Everything that happened to the team, oldest first.
    let log = audit(&admin, &format!("team_id={}", team.id)).await;
    let mut happened = actions(&log);
    happened.reverse();
    assert_eq!(
        happened,
        vec![
            "team_requested",
            "team_approved",
            "member_invited",
            "invitation_accepted",
            "access_created",
            "access_credentials_viewed",
            "access_package_downloaded",
            "access_package_downloaded",
            "access_regenerated",
            "access_credentials_viewed",
            "member_role_changed",
            "event_created",
            "event_cancelled",
            "team_cap_changed",
            "team_suspended",
            "team_reactivated",
            "access_removed",
            "team_deleted",
        ]
    );
    let entries = log["entries"].as_array().unwrap();
    let entry = |action: &str| entries.iter().rev().find(|e| e["action"] == action).cloned().unwrap();
    // Who, on what, from where.
    let approved = entry("team_approved");
    assert_eq!((approved["actor_email"].as_str(), approved["team_name"].as_str()), (Some(admin.email.as_str()), Some(team.name.as_str())));
    assert_eq!(approved["details"]["member_cap"], 6);
    assert_eq!(entry("member_invited")["object"], member.email.as_str());
    assert_eq!(entry("invitation_accepted")["actor_email"], member.email.as_str());
    let created = entry("access_created");
    assert_eq!((created["object"].as_str(), created["details"]["kind"].as_str(), created["actor_email"].as_str()), (Some(username.as_str()), Some("shared"), Some(team.leader.email.as_str())));
    let downloads: Vec<&str> = entries.iter().rev().filter(|e| e["action"] == "access_package_downloaded").map(|e| e["details"]["form"].as_str().unwrap()).collect();
    assert_eq!(downloads, vec!["certificate", "enrollment"]);
    assert_eq!(entry("member_role_changed")["details"]["role"], "leader");
    let cancelled = entry("event_cancelled");
    assert_eq!((cancelled["object"].as_str(), cancelled["details"]["reason"].as_str()), (Some(event.name.as_str()), Some("host")));
    assert_eq!(entry("team_cap_changed")["details"]["member_cap"], 9);
    assert_eq!(entry("team_deleted")["details"]["removed_accesses"], 0, "its only access had been removed before");
    for e in entries {
        let ip = e["ip"].as_str().unwrap();
        assert!(ip.parse::<std::net::IpAddr>().is_ok(), "every entry carries the client's address: {e}");
        assert!(e["occurred_at"].is_string() && !e["actor_email"].as_str().unwrap().is_empty());
    }

    // What is not about a team: the administrator's sign-in, the rescue command that let
    // them in, the check they asked for.
    let by_admin = audit(&admin, &format!("actor={}", admin.email)).await;
    let theirs = actions(&by_admin);
    for expected in ["admin_signed_in", "reconciliation_requested", "team_approved", "team_suspended"] {
        assert!(theirs.contains(&expected), "{expected} by the administrator: {theirs:?}");
    }
    assert!(by_admin["entries"].as_array().unwrap().iter().all(|e| e["actor_email"] == admin.email.as_str()));
    let rescue = audit(&admin, "action=rescue_code_issued").await;
    let issued = rescue["entries"].as_array().unwrap().iter().find(|e| e["object"] == admin.email.as_str()).expect("the rescue command is recorded");
    assert_eq!((issued["actor_email"].as_str(), issued["ip"].as_str()), (Some(""), Some("command-line")));
    // An ordinary sign-in is not an administrator's.
    assert!(audit(&admin, &format!("actor={}", member.email)).await["entries"].as_array().unwrap().iter().all(|e| e["action"] != "admin_signed_in"));

    // No secret anywhere in what was recorded: not the access's passwords, not the password
    // of the certificate store, not the platform's keys.
    let everything = format!("{}{}{}", log, by_admin, rescue);
    for (what, secret) in [("the TAK password", &password), ("the new TAK password", &new_password), ("the store password", &store_password)] {
        assert!(secret.len() >= 12 && !everything.contains(secret.as_str()), "{what} is in the audit log");
    }
    for name in ["APP_SECRET", "AGENT_TOKEN", "LDAP_MANAGER_PW"] {
        assert!(!everything.contains(setting(name)), "{name} is in the audit log");
    }
    let fields: std::collections::BTreeSet<&str> = entries.iter().flat_map(|e| e["details"].as_object().unwrap().keys().map(String::as_str)).collect();
    for field in &fields {
        assert!(!["password", "passphrase", "secret", "token", "code"].iter().any(|word| field.contains(word)), "detail field {field}");
    }

    // Filters and paging.
    let downloads = audit(&admin, &format!("team_id={}&action=access_package_downloaded", team.id)).await;
    assert_eq!((downloads["total"].as_i64(), actions(&downloads).len()), (Some(2), 2));
    let page = admin.get(&format!("/admin/audit?team_id={}&page_size=5&page=2", team.id)).await.ok();
    assert_eq!((page["total"].as_i64(), page["page"].as_i64(), page["entries"].as_array().unwrap().len()), (Some(18), Some(2), 5));
    assert_eq!(page["entries"][0]["action"], "event_cancelled", "the sixth from the end, newest first");
    let tomorrow = iso(utc_in(86_400));
    let none = admin.get(&format!("/admin/audit?team_id={}&from={}", team.id, tomorrow.replace('+', "%2B"))).await.ok();
    assert_eq!(none["total"], 0);
    let until_now = admin.get(&format!("/admin/audit?team_id={}&to={}", team.id, tomorrow.replace('+', "%2B"))).await.ok();
    assert_eq!(until_now["total"], 18);
    admin.get("/admin/audit?action=no_such_action").await.expect_error(400, "invalid_input");
    admin.get("/admin/audit?from=yesterday").await.expect_error(400, "invalid_input");
    // The deleted team is still offered by the filter, under the name it had.
    assert!(log["teams"].as_array().unwrap().iter().any(|t| t["id"] == team.id.as_str() && t["name"] == team.name.as_str()));
    assert!(log["actions"].as_array().unwrap().len() >= 35 && log["retention_days"] == 365);
}

// ------------------------------------------------------------------------------ backups and actions

/// Archives of the backup folder, by name.
fn archives() -> std::collections::BTreeSet<String> {
    std::fs::read_dir(repo_root().join("deploy/backups"))
        .map(|entries| entries.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.starts_with("teamtak-backup-")).collect())
        .unwrap_or_default()
}

/// Lists an archive with `tar`, through `openssl` when it is encrypted.
fn archive_listing(name: &str, passphrase: Option<&str>) -> Result<String, String> {
    let path = repo_root().join("deploy/backups").join(name);
    let script = match passphrase {
        Some(_) => "openssl enc -d -aes-256-cbc -pbkdf2 -iter 200000 -md sha256 -pass env:BACKUP_PASSPHRASE -in \"$ARCHIVE\" | tar -tzf -",
        None => "tar -tzf \"$ARCHIVE\"",
    };
    let output = std::process::Command::new("bash")
        .args(["-o", "pipefail", "-c", script])
        .env("ARCHIVE", &path)
        .env("BACKUP_PASSPHRASE", passphrase.unwrap_or(""))
        .output()
        .expect("bash");
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).into_owned())
    }
}

#[tokio::test]
#[ignore = "needs the running stack"]
async fn a_backup_is_written_on_request_listed_recorded_and_encrypted_when_asked() {
    let admin = Session::admin().await;
    let passphrase = "correct horse battery staple e2e";
    let existing = archives();
    // A plain archive first, whatever an earlier run left in the settings.
    admin.patch("/admin/settings", json!({ "backup_passphrase": "" })).await.ok();

    let done = admin.post("/admin/backups/run", json!({})).await.ok();
    let run = &done["run"];
    assert_eq!((run["ok"].as_bool(), run["trigger"].as_str(), run["requested_by"].as_str()), (Some(true), Some("manual"), Some(admin.email.as_str())), "{run}");
    let plain = run["file"].as_str().unwrap().to_string();
    assert!(plain.starts_with("teamtak-backup-") && plain.ends_with("Z.tar.gz"), "{plain}");
    assert!(run["size_bytes"].as_i64().unwrap() > 100_000 && run["encrypted"] == false);

    // It is on the server's disk and holds the five parts a restore needs.
    let listing = archive_listing(&plain, None).expect("the archive reads back");
    for member in [
        "manifest.txt",
        "ldap-data.ldif",
        "takserver.dump",
        "platform.dump",
        "deploy/.env",
        "deploy/data/tak/CoreConfig.xml",
        &format!("deploy/data/tak/certs/{}.pem", setting("INT_CA_NAME")),
        &format!("deploy/data/tak/certs/{}.key", setting("INT_CA_NAME")),
        "deploy/data/tak/UserAuthenticationFile.xml",
    ] {
        assert!(listing.lines().any(|l| l == member), "{member} missing from the archive");
    }
    assert!(!listing.lines().any(|l| l.starts_with("deploy/data/tak/logs/") && l.ends_with(".log")), "logs are not part of a backup");

    // The dashboard lists it; the audit log has the run.
    let monitoring = fresh_look(&admin).await;
    let backups = &monitoring["snapshot"]["backups"];
    assert_eq!(backups["files"][0]["name"], plain.as_str(), "newest first: {}", backups["files"]);
    assert_eq!((backups["enabled"].as_bool(), backups["passphrase_set"].as_bool()), (Some(true), Some(false)));
    assert_eq!(backups["runs"][0]["file"], plain.as_str());
    assert!(!open_alerts(&monitoring).iter().any(|(kind, _)| kind == "backup_stale" || kind == "backup_failed"));
    let log = audit(&admin, "action=backup_run").await;
    let entry = log["entries"].as_array().unwrap().iter().find(|e| e["details"]["file"] == plain.as_str()).expect("the run is in the audit log");
    assert_eq!((entry["actor_email"].as_str(), entry["details"]["trigger"].as_str(), entry["details"]["outcome"].as_str()), (Some(admin.email.as_str()), Some("manual"), Some("ok")));

    // With a passphrase in the settings the next archive is encrypted. The passphrase is
    // never given back, and is in no audit entry.
    let saved = admin.patch("/admin/settings", json!({ "backup_passphrase": passphrase })).await.ok();
    assert_eq!(saved["backup_passphrase_set"], true);
    assert!(!saved.to_string().contains(passphrase) && !admin.get("/admin/settings").await.ok().to_string().contains(passphrase));
    // Archive names carry the second they were started at.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let done = admin.post("/admin/backups/run", json!({})).await.ok();
    let encrypted = done["run"]["file"].as_str().unwrap().to_string();
    assert!(encrypted.ends_with(".tar.gz.enc") && done["run"]["encrypted"] == true, "{}", done["run"]);
    if std::process::Command::new("openssl").arg("version").output().is_ok() {
        let listing = archive_listing(&encrypted, Some(passphrase)).expect("the archive opens with the passphrase");
        assert!(listing.lines().any(|l| l == "platform.dump") && listing.lines().any(|l| l == "deploy/.env"));
        assert!(archive_listing(&encrypted, Some("another passphrase")).is_err(), "and with no other");
    }
    assert!(archive_listing(&encrypted, None).is_err(), "an encrypted archive is not a readable tar");
    let recorded = format!("{}{}", audit(&admin, "action=settings_changed").await, audit(&admin, "action=backup_run").await);
    assert!(recorded.contains("backup_passphrase") && !recorded.contains(passphrase), "the setting is named, its value is not recorded");

    // As it was, and without the archives this test wrote.
    admin.patch("/admin/settings", json!({ "backup_passphrase": "" })).await.ok();
    for name in archives().difference(&existing) {
        let _ = std::fs::remove_file(repo_root().join("deploy/backups").join(name));
    }
    assert_eq!(archives(), existing);
}

#[tokio::test]
#[ignore = "needs the running stack"]
async fn an_administrator_restarts_a_service_of_this_stack_from_the_api() {
    let admin = Session::admin().await;
    let started = |monitoring: &Value| services(monitoring).iter().find(|s| s["service"] == "mailpit").unwrap()["started_at"].as_str().unwrap().to_string();
    let before = started(&fresh_look(&admin).await);

    // The mail catcher: the one service whose restart disturbs nothing else here.
    let done = admin.post("/admin/services/mailpit/restart", json!({})).await.ok();
    assert_eq!(done["restarted"], true);
    let after = monitoring_until(&admin, "the mail catcher back up", Duration::from_secs(60), |m| {
        services(m).iter().any(|s| s["service"] == "mailpit" && s["state"] == "running" && s["started_at"].as_str() != Some(before.as_str()))
    })
    .await;
    assert_ne!(started(&after), before, "the container really restarted");

    let log = audit(&admin, "action=service_restarted").await;
    let entry = log["entries"].as_array().unwrap().iter().find(|e| e["actor_email"] == admin.email.as_str()).expect("the restart is in the audit log");
    assert_eq!((entry["object"].as_str(), entry["details"]["outcome"].as_str()), (Some("mailpit"), Some("ok")));

    // Wait until it takes e-mail again, so the tests that follow find it ready.
    let deadline = Instant::now() + Duration::from_secs(60);
    while reqwest::Client::new().get(format!("{}/api/v1/info", mailpit_url())).send().await.map(|r| !r.status().is_success()).unwrap_or(true) {
        assert!(Instant::now() < deadline, "the mail catcher did not come back");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let later = Session::new_account("afterrestart").await;
    assert!(later.get("/me").await.ok()["account"]["email"].is_string(), "login codes are delivered again");

    // The Let's Encrypt hand-over is offered only where it is set up: here the agent's
    // script refuses, and the platform says the action failed instead of pretending.
    admin.post("/admin/letsencrypt/sync", json!({})).await.expect_error(502, "action_failed");
    let log = audit(&admin, "action=letsencrypt_synced").await;
    assert!(log["entries"].as_array().unwrap().iter().any(|e| e["actor_email"] == admin.email.as_str() && e["details"]["outcome"] == "failed"));
}
