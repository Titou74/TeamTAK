//! The two halves of the restore proof (`scripts/prove-restore.sh`).
//!
//! `before_the_backup` prepares a stack that has something to lose: teams, TAK accesses,
//! zips already handed out, a revoked certificate. It keeps what a real user would hold (the
//! zips) outside the stack, asks the platform for a backup, and then changes one more thing,
//! so that the stack is ahead of the backup.
//!
//! Between the two, the script destroys the project's Docker volumes and generated files
//! and runs `deploy/scripts/restore.sh` on the archive.
//!
//! `after_the_restore` takes the zips kept aside and checks, against the restored stack,
//! that they still connect to TAK Server with the right group, that what was revoked is
//! still refused, and that the platform is as it was when the backup was taken.
//!
//! Neither is part of the integration suite: they are run by name, by that script.

mod common;

use std::path::PathBuf;
use std::time::Duration;

use common::*;
use serde_json::{json, Value};

const SEES: Duration = Duration::from_secs(15);
const SILENCE: Duration = Duration::from_secs(6);

/// Where the zips and the notes of the first half are kept: outside the stack.
fn state_dir() -> PathBuf {
    let dir = std::env::var("TM_RESTORE_STATE").map(PathBuf::from).unwrap_or_else(|_| repo_root().join("target/restore-proof"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn save_zip(name: &str, zip: &[u8]) {
    std::fs::write(state_dir().join(format!("{name}.zip")), zip).unwrap();
}

fn load_identity(name: &str) -> Identity {
    Package::open(&std::fs::read(state_dir().join(format!("{name}.zip"))).unwrap()).identity()
}

async fn username_of(session: &Session, access: &str) -> String {
    session.get(&format!("/accesses/{access}")).await.ok()["username"].as_str().unwrap().to_string()
}

#[tokio::test]
#[ignore = "run by scripts/prove-restore.sh"]
async fn before_the_backup() {
    let admin = Session::admin().await;

    // Alpha: a shared identity whose zip was handed out to two devices, and a guest.
    let alpha = approved_team(&admin, "shared", 6).await;
    let member = add_member(&alpha).await;
    let shared = create_access(&alpha.leader, &alpha.id, json!({ "kind": "shared" })).await;
    let shared_zip = alpha.leader.package(&shared, "certificate").await;
    save_zip("alpha-shared", &shared_zip);
    let guest = create_access(&alpha.leader, &alpha.id, json!({ "kind": "external", "label": "Guest" })).await;
    save_zip("alpha-guest", &alpha.leader.package(&guest, "certificate").await);

    // Bravo: individual accounts. Its leader's zip was handed out, then regenerated: the
    // first certificate is revoked, the second is the one in use.
    let bravo = approved_team(&admin, "individual", 6).await;
    let own = create_access(&bravo.leader, &bravo.id, json!({ "kind": "individual" })).await;
    save_zip("bravo-revoked", &bravo.leader.package(&own, "certificate").await);
    bravo.leader.post(&format!("/accesses/{own}/regenerate"), json!({})).await.ok();
    save_zip("bravo-current", &bravo.leader.package(&own, "certificate").await);

    // Charlie: an access that was removed before the backup; its certificate must stay dead.
    let charlie = approved_team(&admin, "shared", 6).await;
    let gone = create_access(&charlie.leader, &charlie.id, json!({ "kind": "shared" })).await;
    save_zip("charlie-removed", &charlie.leader.package(&gone, "certificate").await);
    charlie.leader.delete(&format!("/accesses/{gone}")).await.ok();

    // The stack as it is about to be backed up really works.
    let alpha_device = TakClient::connect(&load_identity("alpha-shared"), "before-a").await.expect("alpha connects");
    let alpha_guest = TakClient::connect(&load_identity("alpha-guest"), "before-g").await.expect("alpha's guest connects");
    let bravo_device = TakClient::connect(&load_identity("bravo-current"), "before-b").await.expect("bravo connects");
    assert!(alpha_device.sees(&alpha_guest, SEES).await, "alpha sees its guest before the backup");
    assert!(!bravo_device.sees(&alpha_device, SILENCE).await, "bravo does not see alpha");
    assert!(is_rejected(&load_identity("bravo-revoked")).await, "the regenerated certificate is refused before the backup");
    assert!(is_rejected(&load_identity("charlie-removed")).await, "the removed access is refused before the backup");

    // The backup, asked the way an administrator does.
    let run = admin.post("/admin/backups/run", json!({})).await.ok()["run"].clone();
    assert_eq!(run["ok"], true, "{run}");
    let archive = run["file"].as_str().unwrap().to_string();

    // After the backup, life goes on: Alpha's guest access is regenerated (its zip above is
    // now refused) and a new team appears. The restore must bring back the moment of the
    // backup, not this.
    alpha.leader.post(&format!("/accesses/{guest}/regenerate"), json!({})).await.ok();
    assert!(is_rejected(&load_identity("alpha-guest")).await, "refused once regenerated");
    let late = approved_team(&admin, "shared", 3).await;

    let notes = json!({
        "archive": archive,
        "backup_at": iso(utc_in(0)),
        "admin": admin.email,
        "alpha": { "id": alpha.id, "slug": alpha.slug, "name": alpha.name, "leader": alpha.leader.email, "member": member.email,
                   "shared_access": shared, "shared_username": username_of(&alpha.leader, &shared).await,
                   "guest_access": guest, "guest_username": username_of(&alpha.leader, &guest).await,
                   "shared_fingerprint": load_identity("alpha-shared").fingerprint() },
        "bravo": { "id": bravo.id, "slug": bravo.slug, "name": bravo.name, "leader": bravo.leader.email, "access": own,
                   "username": username_of(&bravo.leader, &own).await },
        "charlie": { "id": charlie.id, "slug": charlie.slug },
        "late": { "id": late.id, "slug": late.slug, "leader": late.leader.email },
        "directory_accounts": ldap_search("ou=People", "(objectClass=inetOrgPerson)", &["uid"]).lines().filter(|l| l.starts_with("uid: ")).count(),
    });
    std::fs::write(state_dir().join("notes.json"), serde_json::to_string_pretty(&notes).unwrap()).unwrap();
    println!("PROOF archive={archive}");
    println!("PROOF state={}", state_dir().display());
}

#[tokio::test]
#[ignore = "run by scripts/prove-restore.sh"]
async fn after_the_restore() {
    let notes: Value = serde_json::from_str(&std::fs::read_to_string(state_dir().join("notes.json")).expect("run before_the_backup first")).unwrap();
    let text = |pointer: &str| notes.pointer(pointer).and_then(Value::as_str).unwrap_or_else(|| panic!("{pointer} missing from the notes")).to_string();
    let (alpha_id, alpha_slug) = (text("/alpha/id"), text("/alpha/slug"));

    // 1. The zip handed out before the backup still connects, with the right group: two
    //    devices of Alpha see each other, and the session TAK Server lists for them is in
    //    Alpha's channel and no other.
    let shared = load_identity("alpha-shared");
    let phone = TakClient::connect(&shared, "after-phone").await.expect("the old zip's certificate is accepted by the restored TAK Server");
    let guest = TakClient::connect(&load_identity("alpha-guest"), "after-guest").await.expect("the guest's zip of the time of the backup is accepted");
    assert!(phone.sees(&guest, SEES).await && guest.sees(&phone, SEES).await, "the two devices of Alpha see each other");
    println!("PROOF old zip of {}-shared connects and exchanges positions inside the team", alpha_slug);
    assert_eq!(ldap_member_of(&text("/alpha/shared_username")), vec![alpha_slug.clone()], "the directory gives the account its team's group");

    // 2. Isolation survived: Bravo, with its own old zip, connects and does not see Alpha.
    let bravo = TakClient::connect(&load_identity("bravo-current"), "after-bravo").await.expect("bravo's current zip is accepted");
    assert!(!bravo.sees(&phone, SILENCE).await && !phone.sees(&bravo, SILENCE).await, "the two teams do not see each other");
    assert_eq!(ldap_member_of(&text("/bravo/username")), vec![text("/bravo/slug")]);
    println!("PROOF another team connects and stays isolated");

    // 3. What was revoked before the backup is still refused.
    assert!(is_rejected(&load_identity("bravo-revoked")).await, "a certificate revoked before the backup must stay refused");
    assert!(is_rejected(&load_identity("charlie-removed")).await, "the certificate of an access removed before the backup must stay refused");
    assert!(ldap_group_members(&text("/charlie/slug")).is_empty(), "the removed access left nothing in the directory");
    println!("PROOF certificates revoked before the backup are still refused");

    // 4. The platform is as it was at the backup. Accounts sign in again (their sessions were
    //    in the database too, but a new sign-in also proves e-mail and the login path).
    let admin = Session::admin().await;
    let leader = Session::login(&text("/alpha/leader"), "en").await;
    let me = leader.get("/me").await.ok();
    assert!(me["teams"].as_array().unwrap().iter().any(|t| t["id"] == alpha_id.as_str() && t["role"] == "leader" && t["status"] == "active"), "{me}");
    let detail = leader.get(&format!("/teams/{alpha_id}")).await.ok();
    let members: Vec<&str> = detail["members"].as_array().unwrap().iter().map(|m| m["email"].as_str().unwrap()).collect();
    assert!(members.contains(&text("/alpha/member").as_str()), "{members:?}");
    assert_eq!(detail["accesses"].as_array().unwrap().len(), 2, "the shared identity and the guest");
    // The same zip comes out again: the stored certificate and the key that encrypts it
    // (APP_SECRET, in deploy/.env) were both restored.
    let again = Package::open(&leader.package(&text("/alpha/shared_access"), "certificate").await).identity();
    assert_eq!(again.fingerprint(), text("/alpha/shared_fingerprint"), "the platform hands out the very certificate it handed out before");
    assert_eq!(again.fingerprint(), shared.fingerprint());
    println!("PROOF the platform hands out the same zip certificate as before the backup");

    // 5. What happened after the backup is gone, as a restore means: the team created
    //    later does not exist, and the guest access regenerated later is back as it was
    //    (which is why its old zip was accepted in step 1).
    let teams = admin.get("/admin/teams").await.ok();
    assert!(!teams["teams"].as_array().unwrap().iter().any(|t| t["id"] == text("/late/id").as_str()), "a team created after the backup does not come back");
    assert!(ldap_group_members(&text("/late/slug")).is_empty());
    println!("PROOF what was done after the backup is not there: the restore is the moment of the backup");

    // 6. The reconciliation finds the directory and TAK Server in line with the database:
    //    both reachable, no error, and nothing corrected since the restore.
    let report = admin.post("/admin/reconciliation/run", json!({})).await.ok();
    assert_eq!((report["directory_reachable"].as_bool(), report["tak_reachable"].as_bool()), (Some(true), Some(true)));
    assert_eq!(report["errors"], json!([]), "the first passes after the restore end without error");
    let backup_at = parse_time(&notes["backup_at"]);
    let since: Vec<&Value> = report["events"].as_array().unwrap().iter().filter(|e| parse_time(&e["occurred_at"]) > backup_at).collect();
    // The only corrections allowed are the ones this very test causes by signing in (none)
    // and connecting devices (sessions are not directory changes): there must be none.
    assert!(since.is_empty(), "the reconciliation corrected something after the restore: {since:?}");
    let directory_now = ldap_search("ou=People", "(objectClass=inetOrgPerson)", &["uid"]).lines().filter(|l| l.starts_with("uid: ")).count();
    println!("PROOF reconciliation after the restore: no error, nothing corrected; {directory_now} accounts in the directory");

    // 7. The audit log came back with the database.
    let log = admin.get(&format!("/admin/audit?team_id={alpha_id}&page_size=50")).await.ok();
    let recorded: Vec<&str> = log["entries"].as_array().unwrap().iter().map(|e| e["action"].as_str().unwrap()).collect();
    assert!(recorded.contains(&"team_approved") && recorded.contains(&"access_package_downloaded"), "{recorded:?}");
    println!("PROOF the audit log of before the backup is there ({} entries for the team)", recorded.len());
}
