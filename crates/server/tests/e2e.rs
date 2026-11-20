//! Integration tests against the real stack: the platform, PostgreSQL, OpenLDAP and TAK
//! Server 5.8 built from the tak.gov ZIP.
//!
//! They need the stack in local mode and are therefore ignored by a plain `cargo test`:
//!
//! ```bash
//! ./install.sh --local
//! scripts/test-integration.sh
//! ```
//!
//! Every test creates its own accounts and teams with random names, so the suite can be
//! replayed on the same stack.

mod common;

use std::time::Duration;

use common::*;
use serde_json::{json, Value};

const SEES: Duration = Duration::from_secs(12);
/// Long enough for two positions to have been relayed if they were going to be.
const SILENCE: Duration = Duration::from_secs(6);

fn ids(list: &Value, key: &str) -> Vec<String> {
    list.as_array().unwrap().iter().map(|v| v[key].as_str().unwrap_or_default().to_string()).collect()
}

// ------------------------------------------------------------------------------ accounts

#[tokio::test]
#[ignore = "needs the running stack"]
async fn login_by_one_time_code() {
    let address = email("login");
    let browser = Session::anonymous();
    browser.get("/me").await.expect_error(401, "unauthenticated");

    // The code arrives by e-mail, in the language of the page it was asked from.
    browser.request_code(&address, Some("fr")).await.ok();
    let mail = latest_mail(&address).await;
    assert!(mail.subject.starts_with("Votre code de connexion"), "{}", mail.subject);
    assert!(mail.text.contains("valable 10 minutes"), "{}", mail.text);
    let code = code_in(&mail);

    // A wrong code is refused and creates nothing.
    let wrong = if code == "000000" { "000001" } else { "000000" };
    browser.post("/auth/verify", json!({ "email": address, "code": wrong })).await.expect_error(400, "invalid_code");
    browser.get("/me").await.expect_error(401, "unauthenticated");

    // The right one opens a session and creates the account, with the page's language.
    let me = browser.post("/auth/verify", json!({ "email": address, "code": code, "language": "fr" })).await.ok();
    assert_eq!(me["account"]["email"], address);
    assert_eq!(me["account"]["language"], "fr");
    assert_eq!(me["account"]["is_admin"], false);
    assert_eq!(browser.get("/me").await.ok()["account"]["email"], address);

    // A code works once.
    Session::anonymous().post("/auth/verify", json!({ "email": address, "code": code })).await.expect_error(400, "invalid_code");

    // Signing out ends the session.
    browser.post("/auth/logout", json!({})).await.ok();
    browser.get("/me").await.expect_error(401, "unauthenticated");
}

#[tokio::test]
#[ignore = "needs the running stack"]
async fn emails_follow_the_language_saved_on_the_account() {
    let account = Session::login(&email("lang"), "en").await;
    assert!(latest_mail(&account.email).await.subject.starts_with("Your sign-in code"));
    account.patch("/me", json!({ "language": "fr" })).await.ok();
    account.patch("/me", json!({ "language": "xx" })).await.expect_error(400, "invalid_input");

    // The next code is written in French, whatever the page it is asked from.
    Session::anonymous().request_code(&account.email, Some("en")).await.ok();
    assert!(latest_mail(&account.email).await.subject.starts_with("Votre code de connexion"));
}

#[tokio::test]
#[ignore = "needs the running stack"]
async fn the_colour_theme_is_saved_on_the_account() {
    let account = Session::login(&email("theme"), "en").await;
    assert_eq!(account.get("/me").await.ok()["account"]["theme"], "auto", "the device decides by default");
    assert_eq!(account.patch("/me", json!({ "theme": "dark" })).await.ok()["account"]["theme"], "dark");
    account.patch("/me", json!({ "theme": "blue" })).await.expect_error(400, "invalid_input");
    // Changing something else leaves the theme alone.
    account.patch("/me", json!({ "display_name": "Night owl" })).await.ok();
    assert_eq!(account.get("/me").await.ok()["account"]["theme"], "dark");
}

#[tokio::test]
#[ignore = "needs the running stack"]
async fn login_codes_are_rate_limited_per_address() {
    let address = email("limit");
    let browser = Session::anonymous();
    let allowed: u64 = setting("LOGIN_MAX_CODES_PER_EMAIL").parse().unwrap();
    for _ in 0..allowed {
        browser.request_code(&address, None).await.ok();
    }
    browser.request_code(&address, None).await.expect_error(429, "rate_limited");
    assert_eq!(mail_count(&address).await, allowed, "no e-mail leaves once the limit is reached");

    // A code stops working after too many wrong guesses, even when the right one follows.
    let victim = email("guess");
    browser.request_code(&victim, None).await.ok();
    let code = code_in(&latest_mail(&victim).await);
    for guess in 0..5 {
        let wrong = format!("{:06}", (code.parse::<u32>().unwrap() + 1 + guess) % 1_000_000);
        browser.post("/auth/verify", json!({ "email": victim, "code": wrong })).await.expect_error(400, "invalid_code");
    }
    browser.post("/auth/verify", json!({ "email": victim, "code": code })).await.expect_error(400, "invalid_code");

    browser.request_code("not-an-address", None).await.expect_error(400, "invalid_email");
}

#[tokio::test]
#[ignore = "needs the running stack"]
async fn a_new_code_waits_for_the_countdown() {
    let address = email("again");
    let browser = Session::anonymous();
    let cooldown: u64 = setting("LOGIN_RESEND_COOLDOWN_SECONDS").parse().unwrap();
    assert!(cooldown > 0, "LOGIN_RESEND_COOLDOWN_SECONDS=0 turns the countdown off: nothing to test");
    // The page is told how long the countdown lasts, to show it.
    assert_eq!(browser.get("/config").await.ok()["resend_cooldown_seconds"], cooldown);

    browser.request_code_once(&address, None).await.ok();
    // Straight away: refused, with the time left, and no second e-mail.
    let refused = browser.request_code_once(&address, None).await;
    refused.expect_error(429, "resend_too_soon");
    let wait = refused.body["retry_after"].as_u64().unwrap();
    assert!((1..=cooldown).contains(&wait), "retry_after {wait}, cooldown {cooldown}");
    assert_eq!(mail_count(&address).await, 1);
    // Another address is not held back.
    browser.request_code_once(&email("other"), None).await.ok();

    // Once the countdown has run out, a new code goes.
    tokio::time::sleep(Duration::from_secs(wait)).await;
    browser.request_code_once(&address, None).await.ok();
    assert_eq!(mail_count(&address).await, 2);
}

#[tokio::test]
#[ignore = "needs the running stack"]
async fn asking_for_a_code_needs_a_solved_challenge() {
    use sha2::{Digest, Sha256};

    let address = email("bot");
    let browser = Session::anonymous();
    // A script that only posts addresses gets nothing.
    browser.post("/auth/request-code", json!({ "email": address })).await.expect_error(400, "challenge_failed");

    // Nor with a number it did not search for.
    let mut wrong = browser.proof().await;
    wrong["number"] = json!(wrong["number"].as_u64().unwrap() + 1);
    browser.post("/auth/request-code", json!({ "email": address, "proof": wrong })).await.expect_error(400, "challenge_failed");

    // Nor with an easy challenge of its own under the server's signature.
    let mut forged = browser.proof().await;
    let salt = forged["salt"].as_str().unwrap().to_string();
    forged["challenge"] = json!(hex::encode(Sha256::digest(format!("{salt}0"))));
    forged["number"] = json!(0);
    browser.post("/auth/request-code", json!({ "email": address, "proof": forged })).await.expect_error(400, "challenge_failed");
    assert_eq!(mail_count(&address).await, 0);

    // A solved challenge is good for one code.
    let proof = browser.proof().await;
    browser.post("/auth/request-code", json!({ "email": address, "proof": proof })).await.ok();
    let other = email("bot");
    browser.post("/auth/request-code", json!({ "email": other, "proof": proof })).await.expect_error(400, "challenge_failed");
    assert_eq!(mail_count(&address).await, 1);
    assert_eq!(mail_count(&other).await, 0);
}

#[tokio::test]
#[ignore = "needs the running stack"]
async fn state_changing_requests_need_the_csrf_header() {
    // A form posted from another site carries no custom header: it must be refused.
    let response = reqwest::Client::new()
        .post(format!("{}/api/auth/request-code", base_url()))
        .json(&json!({ "email": email("csrf") }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 403);
}

#[tokio::test]
#[ignore = "needs the running stack"]
async fn an_account_without_a_team_can_do_nothing() {
    let admin = Session::admin().await;
    let team = approved_team(&admin, "shared", 5).await;
    let access = create_access(&team.leader, &team.id, json!({ "kind": "shared" })).await;

    let outsider = Session::new_account("outsider").await;
    let me = outsider.get("/me").await.ok();
    assert_eq!(me["teams"].as_array().unwrap().len(), 0);

    // Another team does not exist for them, nor does its access.
    outsider.get(&format!("/teams/{}", team.id)).await.expect_error(404, "not_found");
    outsider.get(&format!("/accesses/{access}")).await.expect_error(404, "not_found");
    outsider
        .post(&format!("/teams/{}/accesses", team.id), json!({ "kind": "external", "label": "me" }))
        .await
        .expect_error(404, "not_found");
    outsider
        .post(&format!("/teams/{}/invitations", team.id), json!({ "email": outsider.email }))
        .await
        .expect_error(404, "not_found");
    outsider.post(&format!("/accesses/{access}/regenerate"), json!({})).await.expect_error(404, "not_found");
    outsider.get("/admin/teams").await.expect_error(403, "forbidden");
    outsider.post(&format!("/admin/teams/{}/decision", team.id), json!({ "approve": true })).await.expect_error(403, "forbidden");

    // Their own request gives nothing until an administrator approves it.
    let pending = outsider.post("/teams", json!({ "name": unique("Pending "), "mode": "individual" })).await.ok();
    let pending_id = pending["id"].as_str().unwrap();
    assert_eq!(pending["status"], "pending");
    outsider
        .post(&format!("/teams/{pending_id}/accesses"), json!({ "kind": "individual" }))
        .await
        .expect_error(409, "team_not_active");
    outsider
        .post(&format!("/teams/{pending_id}/invitations"), json!({ "email": email("x") }))
        .await
        .expect_error(409, "team_not_active");
    assert!(!ldap_group_members(pending["slug"].as_str().unwrap()).iter().any(|_| true), "a pending team has no group");
}

// ------------------------------------------------------------------------------ teams

#[tokio::test]
#[ignore = "needs the running stack"]
async fn team_request_approval_and_invitations_within_the_cap() {
    let admin = Session::admin().await;
    let leader = Session::new_account("leader").await;
    let name = unique("Équipe Montagne ");

    // External accesses are not a connection mode: they fit both modes.
    leader.post("/teams", json!({ "name": name, "mode": "external" })).await.expect_error(400, "invalid_input");

    // Request: nothing but a pending row, and a notification to the administrators.
    let team = leader.post("/teams", json!({ "name": name, "mode": "individual", "message": "Secours en montagne" })).await.ok();
    let (id, slug) = (team["id"].as_str().unwrap(), team["slug"].as_str().unwrap());
    assert!(slug.starts_with("equipe-montagne-"), "accents folded in the TAK group name: {slug}");
    leader.post("/teams", json!({ "name": name, "mode": "individual" })).await.expect_error(409, "team_name_taken");
    assert!(latest_mail(&admin.email).await.subject.contains(&name));

    // Approval by an administrator, who sets the member cap.
    let listed = admin.get("/admin/teams").await.ok();
    let row = listed["teams"].as_array().unwrap().iter().find(|t| t["id"] == id).expect("request listed").clone();
    assert_eq!((row["status"].as_str(), row["requester_email"].as_str()), (Some("pending"), Some(leader.email.as_str())));
    leader.post(&format!("/admin/teams/{id}/decision"), json!({ "approve": true })).await.expect_error(403, "forbidden");
    admin.post(&format!("/admin/teams/{id}/decision"), json!({ "approve": true, "member_cap": 3 })).await.ok();
    admin
        .post(&format!("/admin/teams/{id}/decision"), json!({ "approve": false }))
        .await
        .expect_error(409, "team_already_decided");
    assert!(latest_mail(&leader.email).await.subject.contains("is approved"));

    // The leader invites without any administrator: two seats left under a cap of three.
    let (first, second, third) = (email("m1"), email("m2"), email("m3"));
    let invited = leader.post(&format!("/teams/{id}/invitations"), json!({ "email": first, "language": "fr" })).await.ok();
    assert_eq!(invited["email_sent"], true);
    let mail = latest_mail(&first).await;
    assert!(
        mail.subject.contains("vous invite dans l'équipe"),
        "invitee without an account: the inviter's choice of language: {}",
        mail.subject
    );
    leader.post(&format!("/teams/{id}/invitations"), json!({ "email": first })).await.expect_error(409, "already_invited");
    leader.post(&format!("/teams/{id}/invitations"), json!({ "email": leader.email })).await.expect_error(409, "already_member");
    leader.post(&format!("/teams/{id}/invitations"), json!({ "email": second })).await.ok();
    leader.post(&format!("/teams/{id}/invitations"), json!({ "email": third })).await.expect_error(409, "team_full");

    // The invitee signs in with the invited address and accepts; nobody else can.
    let member = Session::login(&first, "fr").await;
    let invitation = member.get("/me").await.ok()["invitations"][0]["id"].as_str().unwrap().to_string();
    let stranger = Session::new_account("stranger").await;
    stranger.post(&format!("/invitations/{invitation}/answer"), json!({ "accept": true })).await.expect_error(404, "not_found");
    member.post(&format!("/invitations/{invitation}/answer"), json!({ "accept": true })).await.ok();
    member
        .post(&format!("/invitations/{invitation}/answer"), json!({ "accept": true }))
        .await
        .expect_error(409, "invitation_not_pending");

    // A member sees the team but manages nothing.
    let detail = member.get(&format!("/teams/{id}")).await.ok();
    assert_eq!(detail["my_role"], "member");
    assert_eq!(detail["members"].as_array().unwrap().len(), 2);
    assert_eq!(detail["invitations"].as_array().unwrap().len(), 0, "pending invitations are for those who manage members");
    member.post(&format!("/teams/{id}/invitations"), json!({ "email": third })).await.expect_error(403, "forbidden");
    member.patch(&format!("/teams/{id}"), json!({ "mode": "shared" })).await.expect_error(403, "forbidden");
    member.delete(&format!("/teams/{id}")).await.expect_error(403, "forbidden");

    // A declined invitation frees its seat.
    let decliner = Session::login(&second, "en").await;
    let pending = decliner.get("/me").await.ok()["invitations"][0]["id"].as_str().unwrap().to_string();
    decliner.post(&format!("/invitations/{pending}/answer"), json!({ "accept": false })).await.ok();
    leader.post(&format!("/teams/{id}/invitations"), json!({ "email": third })).await.ok();

    // The last leader cannot leave; a promoted member can take over.
    let leader_id = leader.account_id().await;
    let member_id = member.account_id().await;
    leader.delete(&format!("/teams/{id}/members/{leader_id}")).await.expect_error(409, "last_leader");
    leader.patch(&format!("/teams/{id}/members/{member_id}"), json!({ "role": "leader" })).await.ok();
    leader.delete(&format!("/teams/{id}/members/{leader_id}")).await.ok();
    leader.get(&format!("/teams/{id}")).await.expect_error(404, "not_found");
}

#[tokio::test]
#[ignore = "needs the running stack"]
async fn a_rejected_request_frees_its_name() {
    let admin = Session::admin().await;
    let requester = Session::new_account("req").await;
    let name = unique("Rejected ");
    let team = requester.post("/teams", json!({ "name": name, "mode": "shared" })).await.ok();
    let id = team["id"].as_str().unwrap();
    admin.post(&format!("/admin/teams/{id}/decision"), json!({ "approve": false, "note": "duplicate" })).await.ok();
    let mail = latest_mail(&requester.email).await;
    assert!(mail.text.contains("duplicate"), "{}", mail.text);
    assert_eq!(requester.get(&format!("/teams/{id}")).await.ok()["team"]["status"], "rejected");
    requester.post(&format!("/teams/{id}/accesses"), json!({ "kind": "shared" })).await.expect_error(409, "team_not_active");
    // The same name can be requested again.
    requester.post("/teams", json!({ "name": name, "mode": "shared" })).await.ok();
}

// ------------------------------------------------------------------------------ TAK accesses

#[tokio::test]
#[ignore = "needs the running stack"]
async fn zips_carry_only_the_connection() {
    let admin = Session::admin().await;
    let team = approved_team(&admin, "shared", 5).await;
    let access = create_access(&team.leader, &team.id, json!({ "kind": "shared" })).await;
    let detail = team.leader.get(&format!("/accesses/{access}")).await.ok();
    let username = detail["username"].as_str().unwrap();
    assert_eq!(username, format!("{}-shared", team.slug));
    let password = detail["password"].as_str().unwrap();
    assert_eq!(password.len(), 24, "long random password, generated by the platform");
    let host = setting("PUBLIC_HOST");
    let ports = client_ports();
    let main = ports[0];
    assert_eq!(detail["server"]["port"], main);
    assert_eq!(detail["server"]["ports"], json!(ports));

    // QR strings in the formats found in phase 0, the iTAK one for each client port; the
    // connection on an alternative port is named after it.
    assert_eq!(detail["qr"]["itak"][main.to_string()], format!("TeamTAK,{host},{main},ssl"));
    for port in &ports[1..] {
        assert_eq!(detail["qr"]["itak"][port.to_string()], format!("TeamTAK ({port}),{host},{port},ssl"));
    }
    assert_eq!(detail["qr"]["atak"], format!("tak://com.atakmap.app/enroll?host={host}&username={username}&token={password}"));

    // Keys allowed in config.pref: the connection, nothing about the user or the device.
    let allowed = [
        "count",
        "description0",
        "enabled0",
        "connectString0",
        "caLocation0",
        "caPassword0",
        "certificateLocation0",
        "clientPassword0",
        "enrollForCertificateWithTrust0",
        "useAuth0",
        "cacheCreds0",
        "displayServerConnectionWidget",
    ];

    let enrollment = Package::open(&team.leader.package(&access, "enrollment").await);
    assert_eq!(enrollment.names(), ["MANIFEST/manifest.xml", "config.pref", "truststore.p12"]);
    assert_eq!(enrollment.pref_value("connectString0").unwrap(), format!("{host}:{main}:ssl"));
    assert_eq!(enrollment.pref_value("enrollForCertificateWithTrust0").as_deref(), Some("true"));
    assert!(enrollment.pref_value("certificateLocation0").is_none());
    assert_eq!(enrollment.trust_roots().len(), 2, "root and intermediate CA");

    let with_certificate = Package::open(&team.leader.package(&access, "certificate").await);
    let p12 = format!("{username}.p12");
    assert_eq!(with_certificate.names(), ["MANIFEST/manifest.xml", "config.pref", p12.as_str(), "truststore.p12"]);
    assert!(with_certificate.pref_value("enrollForCertificateWithTrust0").is_none());

    for package in [&enrollment, &with_certificate] {
        for key in package.pref_keys() {
            assert!(allowed.contains(&key.as_str()), "unexpected preference {key}");
        }
        let everything =
            format!("{}{}", package.pref(), String::from_utf8_lossy(&package.files["MANIFEST/manifest.xml"])).to_lowercase();
        for forbidden in ["callsign", "locationteam", "atakroletype", "group", "role"] {
            assert!(!everything.contains(forbidden), "{forbidden:?} found in a zip");
        }
    }

    // A zip can connect on any client port, and on no other. The zip of an alternative port
    // carries it in its file name and in the name of the connection.
    for port in &ports {
        let (name, bytes) = team.leader.package_at(&access, "enrollment", &format!("?port={port}")).await;
        let zip = Package::open(&bytes);
        assert_eq!(zip.pref_value("connectString0").unwrap(), format!("{host}:{port}:ssl"));
        if *port == main {
            assert_eq!(name, format!("{username}-enrollment.zip"));
            assert_eq!(zip.pref_value("description0").as_deref(), Some("TeamTAK"));
        } else {
            assert_eq!(name, format!("{username}-enrollment-{port}.zip"));
            assert_eq!(zip.pref_value("description0").unwrap(), format!("TeamTAK ({port})"));
        }
    }
    for port in ["1", "8446", "x"] {
        team.leader.get(&format!("/accesses/{access}/package/enrollment?port={port}")).await.expect_error(400, "invalid_input");
    }

    // The certificate was issued by TAK Server's enrollment, for one year by default, and
    // its expiry is recorded by the platform.
    let identity = with_certificate.identity();
    let (cn, not_before, not_after) = teamtak_core::tak::enroll::inspect_certificate(&identity.chain[0]).unwrap();
    assert_eq!(cn, username);
    let days = (not_after - time::OffsetDateTime::now_utc()).whole_days();
    let expected: i64 = setting("CERT_VALIDITY_DAYS").parse().unwrap();
    assert!((expected - 1..=expected).contains(&days), "valid {days} days from now ({not_before} → {not_after})");
    let detail = team.leader.get(&format!("/accesses/{access}")).await.ok();
    let certificates = detail["certificates"].as_array().unwrap();
    assert_eq!(certificates.len(), 1);
    assert_eq!(certificates[0]["fingerprint"], identity.fingerprint());
    assert_eq!(certificates[0]["source"], "package");
    assert!(certificates[0]["expires_at"].as_str().unwrap().starts_with(&not_after.date().to_string()));

    // Downloading again hands out the same certificate, not a new one.
    let again = Package::open(&team.leader.package(&access, "certificate").await);
    assert_eq!(again.identity().fingerprint(), identity.fingerprint());

    // The directory holds the account in its team's group, with a hashed password.
    assert!(ldap_user_exists(username));
    assert_eq!(ldap_group_members(&team.slug), vec![username.to_string()]);
    assert_eq!(ldap_member_of(username), vec![team.slug.clone()]);
    let stored = ldap_search("ou=People", &format!("(uid={username})"), &["userPassword"]);
    assert!(stored.contains("userPassword::") && !stored.contains(password), "password not stored in clear: {stored}");
}

#[tokio::test]
#[ignore = "needs the running stack"]
async fn members_of_a_team_see_each_other_and_another_team_does_not() {
    let admin = Session::admin().await;

    // Team A, individual accounts: the leader and a member each have their own access.
    let alpha = approved_team(&admin, "individual", 5).await;
    let member = add_member(&alpha).await;
    let leader_access = create_access(&alpha.leader, &alpha.id, json!({ "kind": "individual" })).await;
    let member_access = create_access(&member, &alpha.id, json!({ "kind": "individual" })).await;
    // Each one only reaches their own access; the leader reaches both.
    member.get(&format!("/accesses/{leader_access}")).await.expect_error(404, "not_found");
    alpha.leader.get(&format!("/accesses/{member_access}")).await.ok();
    // A shared access does not fit this mode.
    alpha
        .leader
        .post(&format!("/teams/{}/accesses", alpha.id), json!({ "kind": "shared" }))
        .await
        .expect_error(409, "access_kind_not_allowed");

    // Team B, shared identity.
    let bravo = approved_team(&admin, "shared", 5).await;
    let bravo_access = create_access(&bravo.leader, &bravo.id, json!({ "kind": "shared" })).await;

    let leader_identity = Package::open(&alpha.leader.package(&leader_access, "certificate").await).identity();
    let member_identity = Package::open(&member.package(&member_access, "certificate").await).identity();
    let bravo_identity = Package::open(&bravo.leader.package(&bravo_access, "certificate").await).identity();

    let a1 = TakClient::connect(&leader_identity, "alpha-leader").await.expect("alpha leader connects");
    let a2 = TakClient::connect(&member_identity, "alpha-member").await.expect("alpha member connects");
    let b1 = TakClient::connect(&bravo_identity, "bravo").await.expect("bravo connects");

    assert!(a1.sees(&a2, SEES).await, "the leader of alpha receives the member's position");
    assert!(a2.sees(&a1, SEES).await, "the member of alpha receives the leader's position");

    // Isolation: after both teams have been sending for a while, nothing crossed.
    assert!(!b1.sees(&a1, SILENCE).await, "bravo does not receive alpha");
    let since = std::time::Instant::now() - SILENCE;
    assert!(
        !b1.saw_since(&a2, since) && !a1.saw_since(&b1, since) && !a2.saw_since(&b1, since),
        "no traffic between the two teams"
    );
    assert!(!a1.is_closed() && !a2.is_closed() && !b1.is_closed());

    // An external access joins the team's channel like any member.
    let guest_access = create_access(&alpha.leader, &alpha.id, json!({ "kind": "external", "label": "Pilote Hélico" })).await;
    let guest_detail = alpha.leader.get(&format!("/accesses/{guest_access}")).await.ok();
    assert_eq!(guest_detail["username"], format!("{}-x-pilote-helico", alpha.slug));
    member.get(&format!("/accesses/{guest_access}")).await.expect_error(404, "not_found");
    let guest = TakClient::connect(&Package::open(&alpha.leader.package(&guest_access, "certificate").await).identity(), "guest")
        .await
        .expect("guest connects");
    assert!(guest.sees(&a2, SEES).await && a1.sees(&guest, SEES).await, "the guest is in alpha's group");
    assert!(!b1.sees(&guest, SILENCE).await, "and still invisible to bravo");
}

#[tokio::test]
#[ignore = "needs the running stack"]
async fn every_client_port_leads_to_tak_server() {
    let ports = client_ports();
    let admin = Session::admin().await;
    let team = approved_team(&admin, "shared", 5).await;
    let access = create_access(&team.leader, &team.id, json!({ "kind": "shared" })).await;
    let identity = Package::open(&team.leader.package(&access, "certificate").await).identity();

    // One device per port, all on the team's identity: each one receives the first one's
    // position, and the first one receives theirs.
    let mut devices = Vec::new();
    for port in &ports {
        let device = TakClient::connect_on(&identity, &format!("port{port}"), *port).await;
        devices.push(device.unwrap_or_else(|e| panic!("a device connects on port {port}: {e}")));
    }
    let first = &devices[0];
    for (device, port) in devices.iter().zip(&ports).skip(1) {
        assert!(device.sees(first, SEES).await, "the device on {port} receives the one on {}", ports[0]);
        assert!(first.sees(device, SEES).await, "the device on {} receives the one on {port}", ports[0]);
    }
    assert!(devices.iter().all(|d| !d.is_closed()));
}

#[tokio::test]
#[ignore = "needs the running stack"]
async fn regenerating_a_shared_identity_cuts_every_old_certificate() {
    let admin = Session::admin().await;
    let team = approved_team(&admin, "shared", 5).await;
    let member = add_member(&team).await;
    let access = create_access(&team.leader, &team.id, json!({ "kind": "shared" })).await;

    // Every member reaches the shared access, but only a leader may regenerate it.
    let before = member.get(&format!("/accesses/{access}")).await.ok();
    let (username, old_password) =
        (before["username"].as_str().unwrap().to_string(), before["password"].as_str().unwrap().to_string());
    member.post(&format!("/accesses/{access}/regenerate"), json!({})).await.expect_error(403, "forbidden");
    member.delete(&format!("/accesses/{access}")).await.expect_error(403, "forbidden");

    // Three devices on the shared identity: two with the certificate of the zip, one that
    // enrolled by itself with the username and password (QR code or manual entry).
    let package = Package::open(&member.package(&access, "certificate").await);
    let zip_identity = package.identity();
    let device = unique("device-");
    let replaced_identity = device_enroll(&package.trust_roots(), &username, &old_password, &device)
        .await
        .expect("a device enrolls with the credentials");
    // The same device enrolls a second time. TAK Server calls the first certificate
    // "replaced" and drops it from its list of active certificates, but still accepts it.
    let enrolled_identity = device_enroll(&package.trust_roots(), &username, &old_password, &device).await.unwrap();
    assert_ne!(zip_identity.fingerprint(), enrolled_identity.fingerprint());
    assert!(!is_rejected(&replaced_identity).await, "a replaced certificate still connects until it is revoked");
    let phone1 = TakClient::connect(&zip_identity, "phone1").await.unwrap();
    let phone2 = TakClient::connect(&zip_identity, "phone2").await.unwrap();
    let phone3 = TakClient::connect(&enrolled_identity, "phone3").await.unwrap();
    assert!(phone1.sees(&phone2, SEES).await && phone2.sees(&phone3, SEES).await && phone3.sees(&phone1, SEES).await);
    assert!(!phone1.is_closed() && !phone2.is_closed() && !phone3.is_closed(), "one certificate, several devices, no eviction");
    // A wrong password does not enroll.
    assert_eq!(device_enroll(&package.trust_roots(), &username, "wrong-password", &device).await.err(), Some(401));

    // The platform learns about the certificate the device enrolled by itself.
    admin.post("/admin/reconciliation/run", json!({})).await.ok();
    let known = team.leader.get(&format!("/accesses/{access}")).await.ok();
    let sources: Vec<String> = ids(&known["certificates"], "source");
    assert_eq!(
        known["certificates"].as_array().unwrap().len(),
        3,
        "the zip's, the device's, and the one it replaced: {}",
        known["certificates"]
    );
    assert!(sources.contains(&"package".to_string()) && sources.contains(&"device".to_string()));
    assert!(
        known["certificates"].as_array().unwrap().iter().all(|c| c["expires_at"].is_string()),
        "expiry recorded for monitoring"
    );

    // Regenerate.
    let started = std::time::Instant::now();
    let result = team.leader.post(&format!("/accesses/{access}/regenerate"), json!({})).await.ok();
    assert_eq!(result["reenrolled"], true);

    // Every open connection is cut, at once.
    for (name, phone) in [("phone1", &phone1), ("phone2", &phone2), ("phone3", &phone3)] {
        let cut = phone.closed_within(Duration::from_secs(10)).await;
        assert!(cut.is_some(), "{name} is still connected after the regeneration");
    }
    println!("all three devices were cut within {:.1?} of the request", started.elapsed());

    // Neither old certificate comes back, and the old password no longer enrolls.
    assert!(is_rejected(&zip_identity).await, "the old zip certificate is revoked");
    assert!(is_rejected(&enrolled_identity).await, "the certificate the device enrolled is revoked");
    assert!(is_rejected(&replaced_identity).await, "and so is the one it had replaced");
    assert_eq!(device_enroll(&package.trust_roots(), &username, &old_password, &device).await.err(), Some(401));

    // The new zip works, with a new certificate and a new password on the same account.
    let after = team.leader.get(&format!("/accesses/{access}")).await.ok();
    assert_eq!(after["username"], username.as_str());
    assert_ne!(after["password"], old_password.as_str());
    assert_eq!(after["generation"], 2);
    let new_identity = Package::open(&member.package(&access, "certificate").await).identity();
    assert_ne!(new_identity.fingerprint(), zip_identity.fingerprint());
    assert_eq!(ids(&after["certificates"], "fingerprint"), vec![new_identity.fingerprint()]);
    let new1 = TakClient::connect(&new_identity, "new1").await.expect("the new certificate connects");
    let new2 = TakClient::connect(&new_identity, "new2").await.unwrap();
    assert!(new1.sees(&new2, SEES).await, "the regenerated identity works");
    let reenrolled = device_enroll(&package.trust_roots(), &username, after["password"].as_str().unwrap(), &device).await;
    assert!(reenrolled.is_ok(), "the new password enrolls");
}

#[tokio::test]
#[ignore = "needs the running stack"]
async fn removing_an_access_revokes_then_deletes() {
    let admin = Session::admin().await;
    let team = approved_team(&admin, "individual", 5).await;
    let member = add_member(&team).await;
    let leader_access = create_access(&team.leader, &team.id, json!({ "kind": "individual" })).await;
    let member_access = create_access(&member, &team.id, json!({ "kind": "individual" })).await;
    let member_username = member.get(&format!("/accesses/{member_access}")).await.ok()["username"].as_str().unwrap().to_string();

    let leader_identity = Package::open(&team.leader.package(&leader_access, "certificate").await).identity();
    let member_identity = Package::open(&member.package(&member_access, "certificate").await).identity();
    let leader_phone = TakClient::connect(&leader_identity, "leader").await.unwrap();
    let member_phone = TakClient::connect(&member_identity, "member").await.unwrap();
    assert!(leader_phone.sees(&member_phone, SEES).await);

    // The leader removes the member: the member's access goes with them.
    let member_id = member.account_id().await;
    let removed = team.leader.delete(&format!("/teams/{}/members/{member_id}", team.id)).await.ok();
    assert_eq!((removed["removed_accesses"].as_u64(), removed["synced"].as_bool()), (Some(1), Some(true)));

    assert!(member_phone.closed_within(Duration::from_secs(10)).await.is_some(), "the removed member is disconnected");
    assert!(is_rejected(&member_identity).await, "and cannot reconnect");
    assert!(!ldap_user_exists(&member_username), "the account is gone from the directory");
    assert!(!ldap_group_members(&team.slug).contains(&member_username));
    member.get(&format!("/teams/{}", team.id)).await.expect_error(404, "not_found");
    assert!(!leader_phone.is_closed(), "the rest of the team is untouched");

    // A new access for the same person gets a new username: the old one is retired for good.
    let rejoined = add_member(&team).await;
    let _ = rejoined;
    team.leader.post(&format!("/teams/{}/invitations", team.id), json!({ "email": member.email })).await.ok();
    let me = member.get("/me").await.ok();
    let invitation = me["invitations"][0]["id"].as_str().unwrap().to_string();
    member.post(&format!("/invitations/{invitation}/answer"), json!({ "accept": true })).await.ok();
    let again = create_access(&member, &team.id, json!({ "kind": "individual" })).await;
    let new_username = member.get(&format!("/accesses/{again}")).await.ok()["username"].as_str().unwrap().to_string();
    assert_ne!(new_username, member_username);
    assert!(is_rejected(&member_identity).await, "the old certificate does not match the new account");

    // Deleting the team removes everything it had in TAK Server.
    let deleted = team.leader.delete(&format!("/teams/{}", team.id)).await.ok();
    assert_eq!(deleted["synced"], true);
    assert!(leader_phone.closed_within(Duration::from_secs(10)).await.is_some());
    assert!(ldap_group_members(&team.slug).is_empty(), "the group disappears with its last member");
    assert!(!ldap_user_exists(&new_username));
    assert!(is_rejected(&leader_identity).await);
}

#[tokio::test]
#[ignore = "needs the running stack"]
async fn changing_the_mode_removes_the_accesses_that_do_not_fit() {
    let admin = Session::admin().await;
    let team = approved_team(&admin, "shared", 5).await;
    let shared = create_access(&team.leader, &team.id, json!({ "kind": "shared" })).await;
    let guest = create_access(&team.leader, &team.id, json!({ "kind": "external", "label": "Guest" })).await;
    let shared_identity = Package::open(&team.leader.package(&shared, "certificate").await).identity();
    let phone = TakClient::connect(&shared_identity, "shared").await.unwrap();
    let phone2 = TakClient::connect(&shared_identity, "shared2").await.unwrap();
    assert!(phone.sees(&phone2, SEES).await, "both devices are up before the change");
    team.leader.patch(&format!("/teams/{}", team.id), json!({ "mode": "external" })).await.expect_error(400, "invalid_input");

    let changed = team.leader.patch(&format!("/teams/{}", team.id), json!({ "mode": "individual" })).await.ok();
    assert_eq!(changed["removed_accesses"], 1);
    assert!(phone.closed_within(Duration::from_secs(10)).await.is_some(), "the shared identity is revoked");
    assert!(phone2.closed_within(Duration::from_secs(10)).await.is_some(), "on every device");
    assert!(is_rejected(&shared_identity).await);
    team.leader.get(&format!("/accesses/{shared}")).await.expect_error(404, "not_found");
    team.leader.get(&format!("/accesses/{guest}")).await.ok();
    assert_eq!(ldap_group_members(&team.slug), vec![format!("{}-x-guest", team.slug)], "the external access stays");
}

// ------------------------------------------------------------------------------ reconciliation

#[tokio::test]
#[ignore = "needs the running stack"]
async fn reconciliation_repairs_drift_and_reports_it() {
    let admin = Session::admin().await;
    let team = approved_team(&admin, "shared", 5).await;
    let guest_team = approved_team(&admin, "individual", 5).await;
    let access = create_access(&team.leader, &team.id, json!({ "kind": "shared" })).await;
    create_access(&guest_team.leader, &guest_team.id, json!({ "kind": "external", "label": "Partner" })).await;
    let username = format!("{}-shared", team.slug);
    let other = format!("{}-x-partner", guest_team.slug);

    // A clean directory: a check changes nothing (idempotence).
    let clean = admin.post("/admin/reconciliation/run", json!({})).await.ok();
    assert_eq!(clean["errors"], json!([]));
    assert_eq!((clean["directory_reachable"].as_bool(), clean["tak_reachable"].as_bool()), (Some(true), Some(true)));
    let last_event = clean["events"][0]["id"].as_i64().unwrap_or(0);

    // Someone edits the directory behind the platform's back:
    let intruder = unique("intruder-");
    let stray_group = unique("ghosts-");
    let ldif = format!(
        // 1. a member of another team is slipped into this team's group;
        "dn: {team_group}\nchangetype: modify\nadd: member\nmember: {other_dn}\n\n\
         # 2. an account the platform does not know, in a group of its own;\n\
         dn: {intruder_dn}\nchangetype: add\nobjectClass: top\nobjectClass: person\nobjectClass: organizationalPerson\n\
         objectClass: inetOrgPerson\nuid: {intruder}\ncn: {intruder}\nsn: {intruder}\nuserPassword: an-intruder-password\n\n\
         dn: {stray_dn}\nchangetype: add\nobjectClass: top\nobjectClass: groupOfNames\ncn: {stray_group}\nmember: {intruder_dn}\n\n\
         # 3. the password of the team's account is changed.\n\
         dn: {user_dn}\nchangetype: modify\nreplace: userPassword\nuserPassword: changed-by-hand-1234\n-\n\
         replace: description\ndescription: teamtak-pwtag=tampered\n",
        team_group = group_dn(&team.slug),
        other_dn = user_dn(&other),
        intruder_dn = user_dn(&intruder),
        stray_dn = group_dn(&stray_group),
        user_dn = user_dn(&username),
    );
    let (ok, output) = ldap_modify(&ldif);
    assert!(ok, "tampering failed: {output}");
    assert!(ldap_member_of(&other).contains(&team.slug), "the drift is in place");
    let password = team.leader.get(&format!("/accesses/{access}")).await.ok()["password"].as_str().unwrap().to_string();
    let roots = Package::open(&team.leader.package(&access, "enrollment").await).trust_roots();
    let device = unique("device-");
    assert_eq!(
        device_enroll(&roots, &username, &password, &device).await.err(),
        Some(401),
        "the platform's password no longer works"
    );

    // The next check puts everything back and says what it found.
    let report = admin.post("/admin/reconciliation/run", json!({})).await.ok();
    assert_eq!(report["errors"], json!([]));
    let new_events: Vec<&Value> =
        report["events"].as_array().unwrap().iter().filter(|e| e["id"].as_i64().unwrap() > last_event).collect();
    let found = |kind: &str, subject: &str| {
        new_events.iter().any(|e| e["kind"] == kind && e["subject"] == subject && e["is_drift"] == true)
    };
    assert!(found("member_removed", &other), "{new_events:?}");
    assert!(found("member_removed", &intruder), "{new_events:?}");
    assert!(found("user_deleted", &intruder), "{new_events:?}");
    assert!(found("password_set", &username), "{new_events:?}");
    assert!(report["drift_last_7_days"].as_i64().unwrap() >= 4);

    assert_eq!(ldap_group_members(&team.slug), vec![username.clone()]);
    assert_eq!(ldap_member_of(&other), vec![guest_team.slug.clone()]);
    assert!(!ldap_user_exists(&intruder));
    assert!(ldap_group_members(&stray_group).is_empty());
    assert!(device_enroll(&roots, &username, &password, &device).await.is_ok(), "the platform's password works again");

    // And a second check finds nothing left to do.
    let again = admin.post("/admin/reconciliation/run", json!({})).await.ok();
    let newest = again["events"][0]["id"].as_i64().unwrap();
    let after_repair = report["events"][0]["id"].as_i64().unwrap();
    // The certificate enrolled just above may be recorded, but nothing is changed in TAK.
    assert_eq!(newest, after_repair, "no new correction: {}", again["events"][0]);
}

#[tokio::test]
#[ignore = "needs the running stack"]
async fn suspending_a_team_cuts_its_sessions_and_reactivating_restores_them() {
    let admin = Session::admin().await;
    let team = approved_team(&admin, "shared", 5).await;
    let access = create_access(&team.leader, &team.id, json!({ "kind": "shared" })).await;
    let username = format!("{}-shared", team.slug);
    let identity = Package::open(&team.leader.package(&access, "certificate").await).identity();
    let phone1 = TakClient::connect(&identity, "p1").await.unwrap();
    let phone2 = TakClient::connect(&identity, "p2").await.unwrap();
    assert!(phone1.sees(&phone2, SEES).await);

    // Suspension: the accounts lose their only group. TAK Server never applies "no group"
    // to an open session (phase 0), so the platform cuts the sessions itself.
    team.leader.patch(&format!("/admin/teams/{}", team.id), json!({ "suspended": true })).await.expect_error(403, "forbidden");
    let suspended = admin.patch(&format!("/admin/teams/{}", team.id), json!({ "suspended": true })).await.ok();
    assert_eq!(suspended["synced"], true);
    assert!(phone1.closed_within(Duration::from_secs(10)).await.is_some(), "session cut");
    assert!(phone2.closed_within(Duration::from_secs(10)).await.is_some(), "session cut");
    assert!(ldap_user_exists(&username), "the account is kept");
    assert!(ldap_member_of(&username).is_empty(), "without any group");
    assert!(ldap_group_members(&team.slug).is_empty());

    // The certificate is not revoked: a device can reconnect, but it is alone.
    let lonely1 = TakClient::connect(&identity, "l1").await.expect("still a valid certificate");
    let lonely2 = TakClient::connect(&identity, "l2").await.unwrap();
    assert!(!lonely1.sees(&lonely2, SILENCE).await, "no group, no traffic, even on the same account");
    team.leader
        .post(&format!("/teams/{}/accesses", team.id), json!({ "kind": "external", "label": "x" }))
        .await
        .expect_error(409, "team_not_active");

    // Reactivation: the group comes back. TAK Server never gives a group to a session that
    // was opened without one, so the platform cuts those sessions; a client reconnects by
    // itself and gets the group.
    let resumed = admin.patch(&format!("/admin/teams/{}", team.id), json!({ "suspended": false })).await.ok();
    assert_eq!(resumed["synced"], true);
    assert_eq!(ldap_member_of(&username), vec![team.slug.clone()]);
    assert!(lonely1.closed_within(Duration::from_secs(10)).await.is_some(), "the group-less session is cut");
    assert!(lonely2.closed_within(Duration::from_secs(10)).await.is_some(), "the group-less session is cut");
    let back1 = TakClient::connect(&identity, "b1").await.unwrap();
    let back2 = TakClient::connect(&identity, "b2").await.unwrap();
    assert!(back1.sees(&back2, SEES).await, "after reconnecting, the team sees itself again");
}
