//! Integration tests of the video server (MediaMTX), against the real stack with `VIDEO=on`:
//! the platform, TAK Server and the video server.
//!
//! Like the other integration tests they are black-box and ignored by a plain `cargo test`:
//!
//! ```bash
//! ./install.sh --local                 (VIDEO=on is the default)
//! scripts/test-integration.sh video
//! ```
//!
//! The scripted devices are ffmpeg containers (MediaMTX's own image, with ffmpeg) started on
//! the video server's network: they publish a test picture, or watch for two seconds, the way
//! a drone, a phone or a player does.

mod common;

use std::time::{Duration, Instant};

use common::*;
use serde_json::{json, Value};

/// Longest wait for something the platform does right after a request.
const SOON: Duration = Duration::from_secs(15);

fn require_video() {
    install_crypto();
    let video = env().get("VIDEO").map(String::as_str).unwrap_or("off");
    assert_eq!(video, "on", "these tests need the video server: VIDEO=on in deploy/.env, then ./install.sh");
}

fn port(name: &str, default: u16) -> u16 {
    env().get(name).and_then(|p| p.parse().ok()).unwrap_or(default)
}

/// A declared stream and its keys.
struct Stream {
    id: String,
    path: String,
    publish_key: String,
    view_key: String,
}

// The addresses inside the video network: the video server by its service name, on the
// ports inside its container.
fn rtsp(path: &str, user: &str, key: &str) -> String {
    format!("rtsp://{user}:{key}@mediamtx:8554/{path}")
}
fn rtmp(path: &str, user: &str, key: &str) -> String {
    format!("rtmp://mediamtx:1935/{path}?user={user}&pass={key}")
}
fn srt(action: &str, path: &str, user: &str, key: &str) -> String {
    format!("srt://mediamtx:8890?streamid={action}:{path}:{user}:{key}")
}

async fn detail(session: &Session, stream_id: &str) -> Value {
    session.get(&format!("/video-streams/{stream_id}")).await.ok()
}

async fn declare(session: &Session, team_id: &str, label: &str, event_id: Option<&str>) -> Stream {
    let mut body = json!({ "label": label });
    if let Some(event) = event_id {
        body["event_id"] = json!(event);
    }
    let created = session.post(&format!("/teams/{team_id}/video"), body).await.ok();
    assert_eq!(created["synced"], true, "TAK Server must be told when the call returns");
    let id = created["id"].as_str().unwrap().to_string();
    let shown = detail(session, &id).await;
    Stream {
        path: created["path"].as_str().unwrap().to_string(),
        publish_key: shown["publish"]["key"].as_str().unwrap().to_string(),
        view_key: shown["view"]["key"].as_str().unwrap().to_string(),
        id,
    }
}

/// A stream as the team page lists it, from the team `team_id`; `Null` when it is not listed.
async fn listed(session: &Session, team_id: &str, stream_id: &str) -> Value {
    let video = session.get(&format!("/teams/{team_id}/video")).await.ok();
    for channel in video["channels"].as_array().unwrap() {
        if let Some(stream) = channel["streams"].as_array().unwrap().iter().find(|s| s["id"] == stream_id) {
            return stream.clone();
        }
    }
    Value::Null
}

async fn wait_live(session: &Session, team_id: &str, stream_id: &str) {
    let deadline = Instant::now() + SOON;
    loop {
        let stream = listed(session, team_id, stream_id).await;
        if stream["live"]["ready"] == true {
            return;
        }
        assert!(Instant::now() < deadline, "the stream never showed live: {stream}");
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

/// The feed of a stream as TAK Server lists it to a device; `None` when it is not listed.
async fn feed_seen_by(identity: &Identity, stream_id: &str) -> Option<Value> {
    let uid = format!("teamtak-video-{stream_id}");
    device_video_feeds(identity).await.unwrap_or_else(|e| panic!("video feeds: {e}")).into_iter().find(|c| c["uuid"] == uid.as_str())
}

async fn wait_feed(identity: &Identity, stream_id: &str, present: bool, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if feed_seen_by(identity, stream_id).await.is_some() == present {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

#[tokio::test]
#[ignore]
async fn a_stream_takes_its_own_keys_and_nothing_else() {
    require_video();
    let admin = Session::admin().await;
    let team = approved_team(&admin, "shared", 5).await;
    let stream = declare(&team.leader, &team.id, "Drone 1", None).await;
    assert_eq!(stream.path, format!("{}/drone-1", team.slug), "the name comes from the label");
    assert_ne!(stream.publish_key, stream.view_key);

    // The addresses handed out name the public host and the published ports.
    let shown = detail(&team.leader, &stream.id).await;
    let host = setting("PUBLIC_HOST");
    let (rtsp_port, rtmp_port, srt_port) = (port("VIDEO_RTSP_PORT", 8554), port("VIDEO_RTMP_PORT", 1935), port("VIDEO_SRT_PORT", 8890));
    let (p, v) = (&stream.publish_key, &stream.view_key);
    assert_eq!(shown["publish"]["urls"]["rtsp"], format!("rtsp://publish:{p}@{host}:{rtsp_port}/{}", stream.path));
    assert_eq!(shown["publish"]["urls"]["rtmp"], format!("rtmp://{host}:{rtmp_port}/{}?user=publish&pass={p}", stream.path));
    assert_eq!(shown["publish"]["urls"]["srt"], format!("srt://{host}:{srt_port}?streamid=publish:{}:publish:{p}", stream.path));
    assert_eq!(shown["view"]["urls"]["rtsp"], format!("rtsp://view:{v}@{host}:{rtsp_port}/{}", stream.path));
    assert_eq!(shown["valid"], true);
    // Devices reach the published ports.
    assert!(tokio::net::TcpStream::connect(("127.0.0.1", rtsp_port)).await.is_ok(), "RTSP port {rtsp_port} not published");
    assert!(tokio::net::TcpStream::connect(("127.0.0.1", rtmp_port)).await.is_ok(), "RTMP port {rtmp_port} not published");

    // Refused: a wrong key, the view key, a path nobody declared, no credentials.
    assert!(!publish_for(2, "rtsp", &rtsp(&stream.path, "publish", "wrong")).0, "a wrong key published");
    assert!(!publish_for(2, "rtsp", &rtsp(&stream.path, "view", v)).0, "the view key published");
    assert!(!publish_for(2, "rtsp", &rtsp(&stream.path, "publish", v)).0, "the view key with the publish user");
    let undeclared = format!("{}/undeclared", team.slug);
    assert!(!publish_for(2, "rtsp", &rtsp(&undeclared, "publish", p)).0, "a path nobody declared");
    assert!(!publish_for(2, "rtsp", &format!("rtsp://mediamtx:8554/{}", stream.path)).0, "no credentials");

    // Accepted: the publish key. The stream shows live; the view key watches, the wrong one
    // does not.
    let publisher = Publisher::start("rtsp", &rtsp(&stream.path, "publish", p), 90);
    wait_live(&team.leader, &team.id, &stream.id).await;
    let (ok, out) = watch(&rtsp(&stream.path, "view", v));
    assert!(ok, "watching with the view key: {out}");
    let (ok, out) = watch(&srt("read", &stream.path, "view", v));
    assert!(ok, "watching over SRT: {out}");
    assert!(!watch(&rtsp(&stream.path, "view", "wrong")).0, "a wrong view key watched");
    let live = listed(&team.leader, &team.id, &stream.id).await;
    assert_eq!(live["live"]["protocol"], "rtsp");

    // RTMP and SRT publish too.
    let phone = declare(&team.leader, &team.id, "Phone", None).await;
    let (ok, out) = publish_for(2, "flv", &rtmp(&phone.path, "publish", &phone.publish_key));
    assert!(ok, "publishing over RTMP: {out}");
    let (ok, out) = publish_for(2, "mpegts", &srt("publish", &phone.path, "publish", &phone.publish_key));
    assert!(ok, "publishing over SRT: {out}");
    assert!(!publish_for(2, "flv", &rtmp(&phone.path, "view", &phone.view_key)).0, "the view key published over RTMP");

    // New keys: the device publishing is cut at once, the old keys stop working.
    team.leader.post(&format!("/video-streams/{}/regenerate", stream.id), json!({})).await.ok();
    assert!(publisher.stopped_within(SOON).await, "the publisher was not cut");
    let fresh = detail(&team.leader, &stream.id).await;
    let new_key = fresh["publish"]["key"].as_str().unwrap().to_string();
    assert_ne!(new_key, *p);
    assert_ne!(fresh["view"]["key"].as_str().unwrap(), v.as_str());
    assert!(!publish_for(2, "rtsp", &rtsp(&stream.path, "publish", p)).0, "the old publish key still works");
    let (ok, out) = publish_for(2, "rtsp", &rtsp(&stream.path, "publish", &new_key));
    assert!(ok, "the new publish key: {out}");

    // Deleted: its keys are refused for good.
    team.leader.delete(&format!("/video-streams/{}", stream.id)).await.ok();
    assert!(!publish_for(2, "rtsp", &rtsp(&stream.path, "publish", &new_key)).0, "a deleted stream took its key");
    team.leader.get(&format!("/video-streams/{}", stream.id)).await.expect_error(404, "not_found");
}

#[tokio::test]
#[ignore]
async fn the_keys_belong_to_the_team() {
    require_video();
    let admin = Session::admin().await;
    let team = approved_team(&admin, "shared", 5).await;
    let member = add_member(&team).await;
    let other = approved_team(&admin, "shared", 5).await;

    let leaders = declare(&team.leader, &team.id, "Drone 1", None).await;
    // A member declares a stream of their own, and sees the keys of every stream of the team.
    let members = declare(&member, &team.id, "My phone", None).await;
    assert_eq!(detail(&member, &leaders.id).await["publish"]["key"], leaders.publish_key.as_str());
    assert_eq!(listed(&member, &team.id, &members.id).await["can_manage"], true);
    assert_eq!(listed(&member, &team.id, &leaders.id).await["can_manage"], false);
    assert_eq!(detail(&member, &leaders.id).await["can_manage"], false);
    // They manage theirs, not the leader's; the leader manages both.
    member.delete(&format!("/video-streams/{}", leaders.id)).await.expect_error(403, "forbidden");
    member.post(&format!("/video-streams/{}/regenerate", leaders.id), json!({})).await.expect_error(403, "forbidden");
    member.post(&format!("/video-streams/{}/regenerate", members.id), json!({})).await.ok();
    assert_eq!(detail(&team.leader, &members.id).await["can_manage"], true);

    // Another team does not even see them; an administrator sees the list, not the keys.
    other.leader.get(&format!("/video-streams/{}", leaders.id)).await.expect_error(404, "not_found");
    other.leader.delete(&format!("/video-streams/{}", leaders.id)).await.expect_error(404, "not_found");
    other.leader.post(&format!("/teams/{}/video", team.id), json!({ "label": "Intruder" })).await.expect_error(404, "not_found");
    admin.get(&format!("/video-streams/{}", leaders.id)).await.expect_error(404, "not_found");
    let seen = listed(&admin, &team.id, &leaders.id).await;
    assert_eq!((seen["can_use"].clone(), seen["can_manage"].clone()), (json!(false), json!(false)));
    admin.post(&format!("/teams/{}/video", team.id), json!({ "label": "Admin's" })).await.expect_error(403, "forbidden");

    // Names.
    team.leader.post(&format!("/teams/{}/video", team.id), json!({ "label": "drone 1" })).await.expect_error(409, "stream_name_taken");
    team.leader.post(&format!("/teams/{}/video", team.id), json!({ "label": "!!!" })).await.expect_error(400, "invalid_stream_name");
    team.leader.post(&format!("/teams/{}/video", team.id), json!({ "label": "  " })).await.expect_error(400, "invalid_input");
    // The same name in another team is another path.
    let same_name = declare(&other.leader, &other.id, "Drone 1", None).await;
    assert_eq!(same_name.path, format!("{}/drone-1", other.slug));

    // Who saw the keys is in the audit log, never the keys.
    let audit = admin.get(&format!("/admin/audit?team_id={}&action=video_stream_keys_viewed", team.id)).await.ok();
    assert!(audit["total"].as_i64().unwrap() >= 2, "{audit}");
    assert!(!audit.to_string().contains(&leaders.publish_key), "a key in the audit log");
}

#[tokio::test]
#[ignore]
async fn atak_lists_a_stream_to_its_channel_only() {
    require_video();
    let admin = Session::admin().await;
    let team = approved_team(&admin, "shared", 5).await;
    let device = access_identity(&team.leader, &team.id, json!({ "kind": "shared" })).await;
    let other = approved_team(&admin, "shared", 5).await;
    let other_device = access_identity(&other.leader, &other.id, json!({ "kind": "shared" })).await;

    let stream = declare(&team.leader, &team.id, "Drone 1", None).await;
    let feed = feed_seen_by(&device, &stream.id).await.expect("the team's device lists the stream");
    let shown = detail(&team.leader, &stream.id).await;
    assert_eq!(feed["feeds"][0]["url"], shown["view"]["urls"]["rtsp"], "the view address, never the publish key");
    assert_eq!(feed["alias"], format!("Drone 1 · {}", team.name));
    assert!(feed_seen_by(&other_device, &stream.id).await.is_none(), "another team's device lists the stream");

    // New keys: the device gets the new address.
    team.leader.post(&format!("/video-streams/{}/regenerate", stream.id), json!({})).await.ok();
    let fresh = detail(&team.leader, &stream.id).await;
    let feed = feed_seen_by(&device, &stream.id).await.unwrap();
    assert_eq!(feed["feeds"][0]["url"], fresh["view"]["urls"]["rtsp"]);

    // A suspended team's streams leave ATAK and the video server; back with the team.
    admin.patch(&format!("/admin/teams/{}", team.id), json!({ "suspended": true })).await.ok();
    let key = fresh["publish"]["key"].as_str().unwrap();
    assert!(!publish_for(2, "rtsp", &rtsp(&stream.path, "publish", key)).0, "a suspended team published");
    admin.patch(&format!("/admin/teams/{}", team.id), json!({ "suspended": false })).await.ok();
    assert!(wait_feed(&device, &stream.id, true, SOON).await, "the stream did not come back");

    team.leader.delete(&format!("/video-streams/{}", stream.id)).await.ok();
    assert!(feed_seen_by(&device, &stream.id).await.is_none(), "a deleted stream stays in ATAK");
}

#[tokio::test]
#[ignore]
async fn an_event_stream_works_while_the_event_runs() {
    require_video();
    let admin = Session::admin().await;
    let host = approved_team(&admin, "shared", 5).await;
    let host_device = access_identity(&host.leader, &host.id, json!({ "kind": "shared" })).await;
    let guest = approved_team(&admin, "shared", 5).await;
    let guest_device = access_identity(&guest.leader, &guest.id, json!({ "kind": "shared" })).await;
    let outsider = approved_team(&admin, "shared", 5).await;
    let outsider_device = access_identity(&outsider.leader, &outsider.id, json!({ "kind": "shared" })).await;

    let event = create_event(&host, Some(20), 60).await;
    invite_to_event(&host, &event, &guest).await;
    answer_event(&guest, &event, true).await;
    // The guest team declares a stream for the event's channel before it starts.
    let stream = declare(&guest.leader, &guest.id, "Drone", Some(&event.id)).await;
    assert_eq!(stream.path, format!("{}/drone", event.group));
    assert_eq!(listed(&guest.leader, &guest.id, &stream.id).await["valid"], false);
    assert!(feed_seen_by(&guest_device, &stream.id).await.is_none(), "listed before the event");
    assert!(!publish_for(2, "rtsp", &rtsp(&stream.path, "publish", &stream.publish_key)).0, "published before the event");
    // Another team of the event sees it once it works, as in ATAK, and never its keys.
    assert_eq!(listed(&host.leader, &host.id, &stream.id).await, Value::Null, "shown to the host before the event");
    host.leader.get(&format!("/video-streams/{}", stream.id)).await.expect_error(404, "not_found");

    // From the start: listed to both teams, and to them only; the publish key works.
    tokio::time::sleep_until(instant_of(event.starts_at).into()).await;
    assert!(wait_feed(&guest_device, &stream.id, true, Duration::from_secs(10)).await, "not listed at the start");
    assert!(wait_feed(&host_device, &stream.id, true, Duration::from_secs(5)).await, "the host team does not list it");
    assert!(feed_seen_by(&outsider_device, &stream.id).await.is_none(), "a team outside the event lists it");
    let seen = listed(&host.leader, &host.id, &stream.id).await;
    assert_eq!((seen["own"].clone(), seen["can_use"].clone(), seen["valid"].clone()), (json!(false), json!(false), json!(true)), "{seen}");
    host.leader.get(&format!("/video-streams/{}", stream.id)).await.expect_error(404, "not_found");
    let publisher = Publisher::start("rtsp", &rtsp(&stream.path, "publish", &stream.publish_key), 120);
    wait_live(&guest.leader, &guest.id, &stream.id).await;
    let (ok, out) = watch(&rtsp(&stream.path, "view", &stream.view_key));
    assert!(ok, "watching during the event: {out}");

    // At the end: gone from ATAK, and the device publishing is cut.
    tokio::time::sleep_until(instant_of(event.ends_at).into()).await;
    assert!(publisher.stopped_within(Duration::from_secs(10)).await, "still publishing after the end");
    assert!(wait_feed(&guest_device, &stream.id, false, Duration::from_secs(5)).await, "still listed after the end");
    assert!(!publish_for(2, "rtsp", &rtsp(&stream.path, "publish", &stream.publish_key)).0, "published after the event");

    // The journal tells the schedule at work from drift.
    let journal = journal_of_group(&admin, &event.group).await;
    for kind in ["video_feed_added", "video_feed_removed", "video_stopped"] {
        let entry = journal.iter().find(|e| e["kind"] == kind).unwrap_or_else(|| panic!("no {kind} in {journal:?}"));
        assert_eq!(entry["is_drift"], false, "{entry}");
        assert_eq!(entry["subject"], stream.path.as_str());
    }
}

#[tokio::test]
#[ignore]
async fn the_video_servers_questions_are_not_answered_from_outside() {
    require_video();
    // What the video server asks the platform goes to its internal port; through the web
    // port the path does not exist.
    let http = reqwest::Client::new();
    let response = http.post(format!("{}/internal/video/auth", base_url())).json(&json!({ "user": "publish" })).send().await.unwrap();
    assert!(!response.status().is_success(), "the hook answered through the web port: {}", response.status());
}
