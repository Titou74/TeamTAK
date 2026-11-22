//! Integration tests of the maps, against the real stack: the platform and TAK Server 5.8's
//! device profiles.
//!
//! Like the other integration tests they are black-box and ignored by a plain `cargo test`:
//!
//! ```bash
//! ./install.sh --local
//! scripts/test-integration.sh maps
//! ```
//!
//! A scripted device asks TAK Server for its profiles the way ATAK does when "Apply TAK
//! Server profile updates" is on: `GET /Marti/api/device/profile/connection` on 8443 with its
//! certificate, naming the client UID of its streaming connection; TAK Server answers with a
//! data package of the profiles of that connection's groups. A phone enrolling with a
//! username and password gets the enrollment profiles on 8446.
//!
//! Every source is checked before it is stored, by fetching one of its tiles: these tests
//! need the platform to reach IGN's Géoplateforme and OpenTopoMap on the Internet.

mod common;

use std::collections::BTreeMap;
use std::io::Write;
use std::time::{Duration, Instant};

use base64::Engine;
use common::*;
use serde_json::{json, Value};

/// Longest wait for something the platform applies when a request returns.
const SOON: Duration = Duration::from_secs(15);
/// ATAK's preference that TAK Server's profiles need, as the connection zips and the
/// enrollment profile set it.
const PROFILES_PREF: &str = "deviceProfileEnableOnConnect";

fn map_xml(name: &str, url: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<customMapSource>\n    <name>{name}</name>\n    <minZoom>0</minZoom>\n    \
         <maxZoom>19</maxZoom>\n    <tileType>png</tileType>\n    <url>{url}</url>\n</customMapSource>\n"
    )
}

/// A data package of map sources, as ATAK exports one: the files at the root and a manifest.
fn data_package(files: &[(&str, &str)]) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default();
    zip.start_file("MANIFEST/manifest.xml", options).unwrap();
    zip.write_all(b"<MissionPackageManifest version=\"2\"><Configuration/><Contents/></MissionPackageManifest>").unwrap();
    for (name, content) in files {
        zip.start_file(*name, options).unwrap();
        zip.write_all(content.as_bytes()).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

async fn upload(admin: &Session, name: &str, bytes: &[u8]) -> Value {
    let data = base64::engine::general_purpose::STANDARD.encode(bytes);
    admin.post("/admin/map-sources", json!({ "name": name, "data": data })).await.ok()
}

/// What became of each file of an upload: `file → outcome` (`rejected:code` for a refusal).
fn outcomes(answer: &Value) -> BTreeMap<String, String> {
    answer["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            let outcome = r["outcome"].as_str().unwrap().to_string();
            let outcome = match r["problem"]["code"].as_str() {
                Some(code) => format!("{outcome}:{code}"),
                None => outcome,
            };
            (r["file"].as_str().unwrap().to_string(), outcome)
        })
        .collect()
}

fn source_id(answer: &Value, file: &str) -> String {
    let result = answer["results"].as_array().unwrap().iter().find(|r| r["file"] == file).unwrap_or_else(|| panic!("{file}: {answer}"));
    result["id"].as_str().unwrap().to_string()
}

/// The files TAK Server hands a connected device that asks for its profiles, by name; the
/// data package always holds TAK Server's own channel preferences, so an answer that is not
/// one is a failure, not "no map".
async fn on_connect(identity: &Identity, device: &TakClient) -> BTreeMap<String, Vec<u8>> {
    let path = format!("/Marti/api/device/profile/connection?syncSecago=-1&clientUid={}", device.uid);
    let (status, body) = device_api_bytes(identity, &path).await.unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    let files = package_files(&body);
    assert!(files.contains_key("enable-channels.pref"), "TAK Server's own preferences are in every answer: {:?}", files.keys());
    files
}

/// Waits until the device is handed what `wanted` says, and returns what it was handed last.
async fn wait_on_connect(
    identity: &Identity,
    device: &TakClient,
    wanted: impl Fn(&BTreeMap<String, Vec<u8>>) -> bool,
) -> BTreeMap<String, Vec<u8>> {
    let deadline = Instant::now() + SOON;
    loop {
        let files = on_connect(identity, device).await;
        if wanted(&files) || Instant::now() > deadline {
            return files;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// A team in shared mode, its shared access and a device connected with it.
async fn team_with_device(admin: &Session, label: &str) -> (TeamFixture, String, Identity, TakClient) {
    let team = approved_team(admin, "shared", 5).await;
    let access = create_access(&team.leader, &team.id, json!({ "kind": "shared" })).await;
    let package = Package::open(&team.leader.package(&access, "certificate").await);
    assert_eq!(package.pref_value(PROFILES_PREF).as_deref(), Some("true"), "the zip turns on TAK Server's profiles");
    let identity = package.identity();
    let device = TakClient::connect(&identity, label).await.unwrap_or_else(|e| panic!("{label}: {e}"));
    (team, access, identity, device)
}

#[tokio::test]
#[ignore = "needs the stack: ./install.sh --local"]
async fn a_teams_maps_reach_its_devices_and_no_other_teams() {
    install_crypto();
    let admin = Session::admin().await;
    let tag = unique("e2e-");
    let plan_file = format!("{tag}-plan.xml");
    let topo_file = format!("{tag}-topo.xml");
    let plan = map_xml(&format!("Plan {tag}"), "https://data.geopf.fr/tms/1.0.0/GEOGRAPHICALGRIDSYSTEMS.PLANIGNV2/{$z}/{$x}/{$y}.png");
    let topo = map_xml(&format!("Topo {tag}"), "https://tile.opentopomap.org/{$z}/{$x}/{$y}.png");
    // Read as map sources, but they do not work: a layer the provider does not have, and an
    // address inside the stack, which the platform never fetches.
    let wrong = map_xml(&format!("Wrong {tag}"), "https://data.geopf.fr/tms/1.0.0/NOT.A.LAYER/{$z}/{$x}/{$y}.png");
    let inside = map_xml(&format!("Inside {tag}"), "https://takserver:8443/Marti/{$z}/{$x}/{$y}.png");

    // The catalogue takes an ATAK data package as it is: its map sources that work, nothing else.
    let package = data_package(&[
        (plan_file.as_str(), plan.as_str()),
        (topo_file.as_str(), topo.as_str()),
        ("wrong.xml", wrong.as_str()),
        ("inside.xml", inside.as_str()),
        ("readme.txt", "not a map"),
        ("prefs.xml", "<preferences/>"),
    ]);
    let answer = upload(&admin, "maps.zip", &package).await;
    assert_eq!(
        outcomes(&answer),
        BTreeMap::from([
            (plan_file.clone(), "added".to_string()),
            (topo_file.clone(), "added".to_string()),
            ("wrong.xml".to_string(), "rejected:http_status".to_string()),
            ("inside.xml".to_string(), "rejected:private_address".to_string()),
            ("prefs.xml".to_string(), "rejected:not_map_source".to_string()),
        ]),
        "{answer}"
    );
    let wrong_result = answer["results"].as_array().unwrap().iter().find(|r| r["file"] == "wrong.xml").unwrap();
    assert_eq!(wrong_result["problem"]["status"], 400, "the provider's answer is kept");
    let terms: Vec<&str> = answer["results"].as_array().unwrap().iter().filter_map(|r| r["terms"].as_str()).collect();
    assert_eq!(terms, ["open", "light_use"]);
    assert_eq!(outcomes(&upload(&admin, "maps.zip", &package).await)[&plan_file], "unchanged");
    // An upload is larger than the API's other requests: 300 kB pass, and are refused as a map
    // source, not as a request.
    let big = format!("<customMapSource>{}</customMapSource>", " ".repeat(300 * 1024));
    assert_eq!(outcomes(&upload(&admin, "big.xml", big.as_bytes()).await)["big.xml"], "rejected:too_large");
    let plan_id = source_id(&answer, &plan_file);
    let topo_id = source_id(&answer, &topo_file);

    let (alpha, alpha_access, alpha_identity, alpha_device) = team_with_device(&admin, "maps-alpha").await;
    let (bravo, _, bravo_identity, bravo_device) = team_with_device(&admin, "maps-bravo").await;
    let member = add_member(&alpha).await;

    // Only administrators keep the catalogue; only leaders pick the team's maps.
    member.get("/admin/map-sources").await.expect_error(403, "forbidden");
    member.call("PUT", &format!("/teams/{}/maps", alpha.id), Some(json!({ "source_ids": [plan_id] }))).await.expect_error(403, "forbidden");
    let set = alpha.leader.call("PUT", &format!("/teams/{}/maps", alpha.id), Some(json!({ "source_ids": [plan_id] }))).await.ok();
    assert_eq!(set["synced"], true, "TAK Server must have the profile when the call returns");

    // The team's device gets the map; the other team's does not.
    let files = wait_on_connect(&alpha_identity, &alpha_device, |f| f.contains_key(&plan_file)).await;
    assert_eq!(files.get(&plan_file).map(Vec::as_slice), Some(plan.as_bytes()), "{:?}", files.keys());
    let files = on_connect(&bravo_identity, &bravo_device).await;
    assert!(!files.keys().any(|f| f.starts_with(&tag)), "another team's map reached bravo: {:?}", files.keys());

    // A leader adds a source of the team's own: checked, on the team's channel at once, seen by
    // no other team.
    let own_file = format!("{tag}-own.xml");
    let own = map_xml(&format!("Own {tag}"), "https://data.geopf.fr/tms/1.0.0/ORTHOIMAGERY.ORTHOPHOTOS/{$z}/{$x}/{$y}.jpeg");
    let data = base64::engine::general_purpose::STANDARD.encode(own.as_bytes());
    member.post(&format!("/teams/{}/map-sources", alpha.id), json!({ "name": own_file, "data": data })).await.expect_error(403, "forbidden");
    let added = alpha.leader.post(&format!("/teams/{}/map-sources", alpha.id), json!({ "name": own_file, "data": data })).await.ok();
    assert_eq!(outcomes(&added)[&own_file], "added", "{added}");
    assert_eq!(added["synced"], true);
    let own_id = source_id(&added, &own_file);
    let files = wait_on_connect(&alpha_identity, &alpha_device, |f| f.contains_key(&own_file)).await;
    assert!(files.contains_key(&own_file) && files.contains_key(&plan_file), "{:?}", files.keys());
    let bravo_maps = bravo.leader.get(&format!("/teams/{}/maps", bravo.id)).await.ok();
    assert!(!bravo_maps["catalogue"].as_array().unwrap().iter().any(|s| s["id"] == own_id.as_str()), "bravo does not see it");
    assert!(bravo_maps["catalogue"].as_array().unwrap().iter().any(|s| s["id"] == plan_id.as_str()), "but sees the catalogue");
    bravo
        .leader
        .call("PUT", &format!("/teams/{}/maps", bravo.id), Some(json!({ "source_ids": [own_id] })))
        .await
        .expect_error(404, "not_found");
    let catalogue = admin.get("/admin/map-sources").await.ok();
    let listed = catalogue["sources"].as_array().unwrap().iter().find(|s| s["id"] == own_id.as_str()).cloned().unwrap();
    assert_eq!(listed["team_name"], alpha.name.as_str(), "the administrators see whose it is");
    // Checked again on request: it works.
    let checked = alpha.leader.post(&format!("/map-sources/{own_id}/check"), json!({})).await.ok();
    assert_eq!((checked["broken"].as_bool(), checked["problem"].is_null()), (Some(false), true), "{checked}");
    alpha.leader.delete(&format!("/teams/{}/map-sources/{own_id}", alpha.id)).await.ok();
    let files = on_connect(&alpha_identity, &alpha_device).await;
    assert!(!files.contains_key(&own_file), "{:?}", files.keys());

    // The team page lists it; every member downloads it as a data package.
    let maps = member.get(&format!("/teams/{}/maps", alpha.id)).await.ok();
    assert_eq!(maps["channels"][0]["maps"][0]["file_name"], plan_file.as_str());
    assert_eq!(maps["channels"][0]["can_edit"], false, "a member does not pick");
    assert_eq!(maps["catalogue"], json!([]), "nor sees the catalogue");
    let (status, bytes) = member.download(&format!("/teams/{}/maps/package", alpha.id)).await;
    assert_eq!(status, 200);
    assert_eq!(package_files(&bytes).get(&plan_file).map(Vec::as_slice), Some(plan.as_bytes()));

    // A phone that enrolls with the access's username and password gets the preference that
    // makes ATAK ask for the profiles when it connects.
    let detail = alpha.leader.get(&format!("/accesses/{alpha_access}")).await.ok();
    let (username, password) = (detail["username"].as_str().unwrap(), detail["password"].as_str().unwrap());
    let (status, body) = enrollment_profiles(&alpha_identity.roots, username, password, &unique("E2E-enroll-")).await;
    assert_eq!(status, 200);
    let enrolled = package_files(&body);
    let pref = String::from_utf8(enrolled.get("teamtak-device-profiles.pref").cloned().unwrap_or_default()).unwrap();
    assert!(pref.contains(&format!("key=\"{PROFILES_PREF}\" class=\"class java.lang.Boolean\">true<")), "{:?}", enrolled.keys());

    // A file replaced in the catalogue reaches the device in place of the old one.
    let plan_v2 = plan.replace("<maxZoom>19</maxZoom>", "<maxZoom>18</maxZoom>");
    let replaced = upload(&admin, &plan_file, plan_v2.as_bytes()).await;
    assert_eq!(outcomes(&replaced)[&plan_file], "updated");
    assert_eq!(replaced["synced"], true);
    let files = wait_on_connect(&alpha_identity, &alpha_device, |f| f.get(&plan_file).map(Vec::as_slice) == Some(plan_v2.as_bytes())).await;
    assert_eq!(files.get(&plan_file).map(Vec::as_slice), Some(plan_v2.as_bytes()));

    // Taken off the channel: TAK Server no longer hands it out.
    let set = alpha.leader.call("PUT", &format!("/teams/{}/maps", alpha.id), Some(json!({ "source_ids": [] }))).await.ok();
    assert_eq!(set["synced"], true);
    let files = on_connect(&alpha_identity, &alpha_device).await;
    assert!(!files.contains_key(&plan_file), "{:?}", files.keys());
    let (status, _) = member.download(&format!("/teams/{}/maps/package", alpha.id)).await;
    assert_eq!(status, 404, "no map, no package");

    for id in [plan_id, topo_id] {
        admin.delete(&format!("/admin/map-sources/{id}")).await.ok();
    }
}

#[tokio::test]
#[ignore = "needs the stack: ./install.sh --local"]
async fn an_events_maps_reach_its_teams_from_its_start_to_its_end() {
    install_crypto();
    let admin = Session::admin().await;
    let tag = unique("e2e-");
    let file = format!("{tag}-event.xml");
    let xml = map_xml(&format!("Event map {tag}"), "https://tile.opentopomap.org/{$z}/{$x}/{$y}.png");
    let answer = upload(&admin, &file, xml.as_bytes()).await;
    let id = source_id(&answer, &file);

    let (host, _, host_identity, host_device) = team_with_device(&admin, "maps-host").await;
    let (guest, _, guest_identity, guest_device) = team_with_device(&admin, "maps-guest").await;
    let (_outsider, _, outsider_identity, outsider_device) = team_with_device(&admin, "maps-outsider").await;
    // An event lasts at least a minute.
    let event = create_event(&host, Some(20), 60).await;
    invite_to_event(&host, &event, &guest).await;
    answer_event(&guest, &event, true).await;

    // The guest's leaders do not pick the event's maps; the host's do, before it starts.
    let path = format!("/teams/{}/events/{}/maps", guest.id, event.id);
    guest.leader.call("PUT", &path, Some(json!({ "source_ids": [id] }))).await.expect_error(403, "forbidden");
    let path = format!("/teams/{}/events/{}/maps", host.id, event.id);
    assert_eq!(host.leader.get(&path).await.ok()["catalogue"].as_array().unwrap().iter().filter(|s| s["id"] == id.as_str()).count(), 1);
    host.leader.call("PUT", &path, Some(json!({ "source_ids": [id] }))).await.ok();
    let listed = guest.leader.get(&format!("/teams/{}/maps", guest.id)).await.ok();
    let channel = listed["channels"].as_array().unwrap().iter().find(|c| c["group"] == event.group.as_str()).cloned().unwrap();
    assert_eq!((channel["phase"].as_str(), channel["maps"][0]["file_name"].as_str()), (Some("scheduled"), Some(file.as_str())));

    let has_map = |files: &BTreeMap<String, Vec<u8>>| files.contains_key(&file);
    assert!(!has_map(&on_connect(&host_identity, &host_device).await), "not before the start");
    assert!(Instant::now() < instant_of(event.starts_at), "this test ran too slowly to look before the start");

    // Running: both teams get it, the outsider does not.
    tokio::time::sleep_until(instant_of(event.starts_at).into()).await;
    assert!(has_map(&wait_on_connect(&host_identity, &host_device, has_map).await), "the host's device");
    assert!(has_map(&wait_on_connect(&guest_identity, &guest_device, has_map).await), "the guest's device");
    assert!(!has_map(&on_connect(&outsider_identity, &outsider_device).await), "a team outside the event");

    // Over: nobody gets it any more, and none of that was drift.
    tokio::time::sleep_until(instant_of(event.ends_at).into()).await;
    let files = wait_on_connect(&guest_identity, &guest_device, |f| !has_map(f)).await;
    assert!(!has_map(&files), "after the end");
    // The pass writes its journal once it is through, after the device lost the group.
    let deadline = Instant::now() + SOON;
    let profiles = loop {
        let journal = journal_of_group(&admin, &event.group).await;
        let profiles: Vec<(String, bool)> = journal
            .iter()
            .filter(|e| e["kind"].as_str().unwrap_or_default().starts_with("device_profile"))
            .map(|e| (e["kind"].as_str().unwrap().to_string(), e["is_drift"].as_bool().unwrap()))
            .collect();
        if profiles.len() >= 2 || Instant::now() > deadline {
            break profiles;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    let expected = [("device_profile_added".to_string(), false), ("device_profile_removed".to_string(), false)];
    assert_eq!(profiles, expected, "the event's maps come and go with it, which is not drift");
    // An event that is over keeps its maps as they were.
    host.leader.call("PUT", &path, Some(json!({ "source_ids": [] }))).await.expect_error(409, "event_closed");

    admin.delete(&format!("/admin/map-sources/{id}")).await.ok();
}
