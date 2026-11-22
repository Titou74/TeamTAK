//! Integration tests of the team files, against the real stack: the platform and TAK Server
//! 5.8's Enterprise Sync and Data Sync missions.
//!
//! Like the other integration tests they are black-box and ignored by a plain `cargo test`:
//!
//! ```bash
//! ./install.sh --local
//! scripts/test-integration.sh files
//! ```
//!
//! Scripted devices send data packages and create missions with their own certificates, as
//! ATAK does; what TAK Server lists to a device is read with that device's certificate.

mod common;

use std::io::Write;

use common::*;
use serde_json::{json, Value};
use teamtak_core::files::data_package_files;

/// A data package as ATAK sends one: a manifest and a file.
fn data_package(file: &str, content: &str) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default();
    zip.start_file("MANIFEST/manifest.xml", options).unwrap();
    let manifest = format!(
        "<MissionPackageManifest version=\"2\"><Configuration><Parameter name=\"uid\" value=\"{}\"/></Configuration>\
         <Contents><Content ignore=\"false\" zipEntry=\"{file}\"/></Contents></MissionPackageManifest>",
        unique("dp-")
    );
    zip.write_all(manifest.as_bytes()).unwrap();
    zip.start_file(file, options).unwrap();
    zip.write_all(content.as_bytes()).unwrap();
    zip.finish().unwrap().into_inner()
}

/// The data packages TAK Server lists to a device, by name, as ATAK reads the server's list.
async fn listed_to(identity: &Identity) -> Vec<String> {
    let (status, body) = device_request(identity, "GET", "/Marti/sync/search?keywords=missionpackage", None).await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    let body: Value = serde_json::from_slice(&body).unwrap();
    body["results"].as_array().unwrap().iter().map(|r| r["Name"].as_str().unwrap_or_default().to_string()).collect()
}

/// The data packages and the missions a member sees on the team page.
async fn files_of(session: &Session, team_id: &str) -> Value {
    session.get(&format!("/teams/{team_id}/files")).await.ok()
}

fn package<'a>(files: &'a Value, name: &str) -> Option<&'a Value> {
    files["data_packages"].as_array().unwrap().iter().find(|p| p["name"] == name)
}

fn mission<'a>(files: &'a Value, name: &str) -> Option<&'a Value> {
    files["missions"].as_array().unwrap().iter().find(|m| m["name"] == name)
}

/// A team in shared mode, a device connected with its access, and a member.
async fn team(admin: &Session, label: &str) -> (TeamFixture, Identity, TakClient, Session) {
    let team = approved_team(admin, "shared", 5).await;
    let identity = access_identity(&team.leader, &team.id, json!({ "kind": "shared" })).await;
    let device = TakClient::connect(&identity, label).await.unwrap_or_else(|e| panic!("{label}: {e}"));
    let member = add_member(&team).await;
    (team, identity, device, member)
}

#[tokio::test]
#[ignore = "needs the stack: ./install.sh --local"]
async fn data_packages_are_the_teams_and_removed_by_whoever_may() {
    install_crypto();
    let admin = Session::admin().await;
    let (alpha, alpha_identity, alpha_device, member) = team(&admin, "files-alpha").await;
    let (bravo, bravo_identity, _bravo_device, _bravo_member) = team(&admin, "files-bravo").await;
    let tag = unique("e2e-");

    // A device of alpha sends a data package to the server, as ATAK does: for its own groups.
    let from_device = format!("{tag}-device.zip");
    let path = format!("/Marti/sync/missionupload?filename={from_device}&creatorUid={}", alpha_device.uid);
    let (status, body) = device_request(&alpha_identity, "POST", &path, Some((&from_device, data_package("note.txt", "from the field")))).await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));

    // The team's member and leader see it; bravo does not, nor an administrator outside the team.
    let seen = files_of(&member, &alpha.id).await;
    let listed = package(&seen, &from_device).unwrap_or_else(|| panic!("{seen}"));
    assert_eq!(listed["sender"]["kind"], "access", "sent with the team's access");
    assert_eq!(listed["channels"], json!([alpha.slug]));
    assert_eq!(listed["can_delete"], false, "a member removes only what they sent");
    let led = files_of(&alpha.leader, &alpha.id).await;
    assert_eq!(package(&led, &from_device).unwrap()["can_delete"], true, "a leader removes what the team sent");
    assert!(package(&files_of(&bravo.leader, &bravo.id).await, &from_device).is_none());
    admin.get(&format!("/teams/{}/files", alpha.id)).await.expect_error(403, "forbidden");

    // A member sends a plain file: it is wrapped in a data package, stored for alpha's group only.
    let note = format!("{tag}-brief.txt");
    let sent = member.send_file(&format!("/teams/{}/files?name={note}", alpha.id), b"meet at the bridge".to_vec()).await.ok();
    assert_eq!(sent["wrapped"], true);
    let wrapped = sent["name"].as_str().unwrap().to_string();
    assert_eq!(wrapped, format!("{tag}-brief.zip"));
    assert!(listed_to(&alpha_identity).await.contains(&wrapped), "alpha's devices list it");
    assert!(!listed_to(&bravo_identity).await.contains(&wrapped), "bravo's do not");
    // Not to another team's channel.
    member
        .send_file(&format!("/teams/{}/files?name=x.txt&channel={}", alpha.id, bravo.slug), b"x".to_vec())
        .await
        .expect_error(400, "invalid_input");

    // Downloaded through the platform, it is what ATAK imports.
    let seen = files_of(&member, &alpha.id).await;
    let mine = package(&seen, &wrapped).unwrap_or_else(|| panic!("{seen}"));
    assert_eq!((mine["sender"]["kind"].as_str(), mine["can_delete"].as_bool()), (Some("teamtak"), Some(true)));
    let key = mine["key"].as_i64().unwrap();
    let (status, bytes) = member.download(&format!("/teams/{}/files/{key}/content", alpha.id)).await;
    assert_eq!(status, 200);
    assert_eq!(data_package_files(&bytes).unwrap()[&note], b"meet at the bridge");
    // Bravo cannot reach it by its key.
    let (status, _) = bravo.leader.download(&format!("/teams/{}/files/{key}/content", bravo.id)).await;
    assert_eq!(status, 404);

    // Removals: the member may not remove the device's package; the leader may; the member
    // removes their own.
    let device_key = listed["key"].as_i64().unwrap();
    member.delete(&format!("/teams/{}/files/{device_key}", alpha.id)).await.expect_error(403, "forbidden");
    alpha.leader.delete(&format!("/teams/{}/files/{device_key}", alpha.id)).await.ok();
    assert!(!listed_to(&alpha_identity).await.contains(&from_device), "gone from TAK Server");
    member.delete(&format!("/teams/{}/files/{key}", alpha.id)).await.ok();
    assert!(!listed_to(&alpha_identity).await.contains(&wrapped));

    // The administrators' limit applies.
    let before = admin.get("/admin/settings").await.ok()["settings"]["file_upload_max_mb"].clone();
    admin.patch("/admin/settings", json!({ "settings": { "file_upload_max_mb": 1 } })).await.ok();
    let too_large = member.send_file(&format!("/teams/{}/files?name=big.bin", alpha.id), vec![7u8; 1_200_000]).await;
    admin.patch("/admin/settings", json!({ "settings": { "file_upload_max_mb": before } })).await.ok();
    too_large.expect_error(413, "file_too_large");
}

#[tokio::test]
#[ignore = "needs the stack: ./install.sh --local"]
async fn missions_show_their_files_unless_protected() {
    install_crypto();
    let admin = Session::admin().await;
    let (alpha, alpha_identity, alpha_device, member) = team(&admin, "missions-alpha").await;
    let (bravo, _, _, _) = team(&admin, "missions-bravo").await;
    let tag = unique("e2e-");
    let (open, locked, read_only) = (format!("{tag}-open"), format!("{tag}-locked"), format!("{tag}-ro"));

    // A device of alpha creates three missions in ATAK's Data Sync, for alpha's group.
    let create = |name: &str, extra: &str| format!("/Marti/api/missions/{name}?group={}&creatorUid={}{extra}", alpha.slug, alpha_device.uid);
    for (name, extra) in [(&open, ""), (&locked, "&password=s3cret-pass"), (&read_only, "&defaultRole=MISSION_READONLY_SUBSCRIBER")] {
        let (status, body) = device_request(&alpha_identity, "PUT", &create(name, extra), None).await;
        assert!(status == 200 || status == 201, "{name}: {status} {}", String::from_utf8_lossy(&body));
    }

    let seen = files_of(&member, &alpha.id).await;
    let listed = mission(&seen, &open).unwrap_or_else(|| panic!("{seen}"));
    assert_eq!((listed["locked"].as_bool(), listed["writable"].as_bool()), (Some(false), Some(true)));
    assert_eq!(listed["files"], json!([]));
    let protected = mission(&seen, &locked).unwrap();
    assert_eq!((protected["locked"].as_bool(), &protected["files"]), (Some(true), &Value::Null), "nothing of a protected mission");
    assert_eq!(mission(&seen, &read_only).unwrap()["writable"], false);
    assert!(mission(&files_of(&bravo.leader, &bravo.id).await, &open).is_none(), "another team's missions are not listed");

    // A member adds a file to the open mission: the devices of the mission see it.
    let brief = format!("{tag}-brief.pdf");
    // Content of its own: TAK Server names a mission's file after the first upload of the same
    // content, whatever mission it was for.
    let content = format!("%PDF-1.7 brief {tag}");
    let added = member.send_file(&format!("/teams/{}/missions/{open}/files?name={brief}", alpha.id), content.clone().into_bytes()).await.ok();
    let hash = added["hash"].as_str().unwrap().to_string();
    let (status, body) = device_request(&alpha_identity, "GET", &format!("/Marti/api/missions/{open}"), None).await;
    assert_eq!(status, 200);
    let body: Value = serde_json::from_slice(&body).unwrap();
    let names: Vec<&str> = body["data"][0]["contents"].as_array().unwrap().iter().filter_map(|c| c["data"]["name"].as_str()).collect();
    assert!(names.contains(&brief.as_str()), "{names:?}");
    let (status, bytes) = member.download(&format!("/teams/{}/missions/{open}/files/{hash}/content", alpha.id)).await;
    assert_eq!((status, bytes.as_slice()), (200, content.as_bytes()));

    // Nothing goes into a protected or read-only mission, and nothing comes out of a protected one.
    member
        .send_file(&format!("/teams/{}/missions/{locked}/files?name=x.pdf", alpha.id), b"x".to_vec())
        .await
        .expect_error(403, "mission_locked");
    member
        .send_file(&format!("/teams/{}/missions/{read_only}/files?name=x.pdf", alpha.id), b"x".to_vec())
        .await
        .expect_error(409, "mission_read_only");
    let (status, _) = member.download(&format!("/teams/{}/missions/{locked}/files/{hash}/content", alpha.id)).await;
    assert_eq!(status, 403);
    // Bravo cannot reach alpha's mission.
    let (status, _) = bravo.leader.download(&format!("/teams/{}/missions/{open}/files/{hash}/content", bravo.id)).await;
    assert_eq!(status, 404);

    // The member takes their file out again.
    let seen = files_of(&member, &alpha.id).await;
    assert_eq!(mission(&seen, &open).unwrap()["files"][0]["can_remove"], true);
    member.delete(&format!("/teams/{}/missions/{open}/files/{hash}", alpha.id)).await.ok();
    let (_, body) = device_request(&alpha_identity, "GET", &format!("/Marti/api/missions/{open}"), None).await;
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["data"][0]["contents"], json!([]));
}
