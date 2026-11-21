//! Video streams on the video server (MediaMTX): their names and paths, their keys, who may
//! publish or watch, the addresses handed to devices, and the feeds registered in TAK Server.
//!
//! A team declares a stream for one TAK group: its own, or the group of an event it takes
//! part in. The stream's path on the video server is `<group>/<name>`. It has two keys of its
//! own, derived from the application key: one to publish, one to watch. Neither is a TAK
//! password: RTSP, RTMP and SRT carry them unencrypted, and a key that leaks opens that one
//! stream, never a TAK account.
//!
//! A stream is valid while its group exists for its team: the team is active and, for an
//! event's stream, the event runs and the team takes part. Only then does the video server
//! accept its keys, and only then does TAK Server list it among the video feeds of that group,
//! so that ATAK shows it to the group's members and to nobody else.

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use uuid::Uuid;

use crate::login::hashes_equal;
use crate::names::{is_valid_event_group, is_valid_slug, slugify};
use crate::tak::VideoFeed;

/// Longest stream name, the last segment of its path.
pub const MAX_STREAM_NAME_LEN: usize = 32;
/// Longest stream label, the name ATAK shows.
pub const MAX_STREAM_LABEL_LEN: usize = 40;
/// User names that go with the two keys. The path says which stream, the user which key.
pub const PUBLISH_USER: &str = "publish";
pub const VIEW_USER: &str = "view";
/// Prefix of the identifiers of the feeds TeamTAK registers in TAK Server. A feed without it
/// was registered by someone else, and is left alone.
pub const FEED_UID_PREFIX: &str = "teamtak-video-";

/// 32 letters and digits, none of which can be mistaken for another: each character of a key
/// carries 5 bits.
const KEY_ALPHABET: &[u8; 32] = b"abcdefghijkmnpqrstuvwxyz23456789";
/// 24 characters: 120 bits.
const KEY_LEN: usize = 24;

/// The two keys of a stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyRole {
    Publish,
    View,
}

/// A key of a stream: an HMAC of its identifier and generation under the application's key.
/// Nothing is stored; "regenerate" raises the generation, which changes both keys.
pub fn stream_key(mac_key: &[u8], stream_id: Uuid, generation: i32, role: KeyRole) -> String {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(mac_key).expect("HMAC accepts any key length");
    mac.update(b"video-stream\0");
    mac.update(stream_id.as_bytes());
    mac.update(&generation.to_be_bytes());
    mac.update(match role {
        KeyRole::Publish => b"\0publish",
        KeyRole::View => b"\0view",
    });
    // 256 is a multiple of 32: every character is drawn evenly.
    mac.finalize().into_bytes().iter().take(KEY_LEN).map(|b| KEY_ALPHABET[(b % 32) as usize] as char).collect()
}

/// Both keys of a stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamKeys {
    pub publish: String,
    pub view: String,
}

impl StreamKeys {
    pub fn derive(mac_key: &[u8], stream_id: Uuid, generation: i32) -> Self {
        Self {
            publish: stream_key(mac_key, stream_id, generation, KeyRole::Publish),
            view: stream_key(mac_key, stream_id, generation, KeyRole::View),
        }
    }
}

/// A stream label as typed, its spaces collapsed; `None` when it is empty, too long or holds
/// control characters.
pub fn clean_label(raw: &str) -> Option<String> {
    let label = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let length = label.chars().count();
    ((1..=MAX_STREAM_LABEL_LEN).contains(&length) && !label.chars().any(char::is_control)).then_some(label)
}

/// The name of a stream, from its label: the last segment of its path. `None` when the label
/// leaves no letter or digit.
pub fn stream_name(label: &str) -> Option<String> {
    let name = slugify(label, MAX_STREAM_NAME_LEN);
    is_valid_stream_name(&name).then_some(name)
}

/// 1 to 32 characters, `[a-z0-9-]`, not starting or ending with a dash.
pub fn is_valid_stream_name(name: &str) -> bool {
    (1..=MAX_STREAM_NAME_LEN).contains(&name.len())
        && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-')
}

/// Path of a stream on the video server.
pub fn stream_path(group: &str, name: &str) -> String {
    format!("{group}/{name}")
}

/// The group and the name of a path as the video server gives it (`alpha/drone-1`, with or
/// without a leading slash). `None` for anything that cannot be a stream's path.
pub fn split_path(path: &str) -> Option<(&str, &str)> {
    let (group, name) = path.strip_prefix('/').unwrap_or(path).split_once('/')?;
    let group_ok = is_valid_slug(group) || is_valid_event_group(group);
    (group_ok && is_valid_stream_name(name)).then_some((group, name))
}

/// What a client asks the video server, as MediaMTX names it in its authentication requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoAction {
    Publish,
    Read,
    /// Recordings, the control API, metrics: never granted to a stream's keys.
    Other,
}

impl VideoAction {
    pub fn parse(raw: &str) -> Self {
        match raw {
            "publish" => VideoAction::Publish,
            "read" => VideoAction::Read,
            _ => VideoAction::Other,
        }
    }
}

/// Whether `user` and `password` may do `action` on a valid stream that has these keys.
/// Publishing takes the publish key. Watching takes the view key, or the publish key, so that
/// whoever publishes can check the picture.
pub fn may(action: VideoAction, user: &str, password: &str, keys: &StreamKeys) -> bool {
    let publisher = user == PUBLISH_USER && hashes_equal(password, &keys.publish);
    match action {
        VideoAction::Publish => publisher,
        VideoAction::Read => publisher || (user == VIEW_USER && hashes_equal(password, &keys.view)),
        VideoAction::Other => false,
    }
}

/// Where devices reach the video server: the platform's public host and the published ports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoEndpoints {
    pub host: String,
    pub rtsp_port: u16,
    pub rtmp_port: u16,
    /// UDP.
    pub srt_port: u16,
}

/// The address of a stream in each protocol, with a key in it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct StreamUrls {
    pub rtsp: String,
    pub rtmp: String,
    pub srt: String,
}

impl VideoEndpoints {
    /// The host as it goes in a URL: an IPv6 address between brackets.
    fn url_host(&self) -> String {
        if self.host.contains(':') && !self.host.starts_with('[') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        }
    }

    /// The addresses to publish to (a drone, a phone, OBS), with the publish key.
    pub fn publish_urls(&self, path: &str, key: &str) -> StreamUrls {
        self.urls(path, PUBLISH_USER, key, "publish")
    }

    /// The addresses to watch from (ATAK, iTAK, VLC), with the view key.
    pub fn view_urls(&self, path: &str, key: &str) -> StreamUrls {
        self.urls(path, VIEW_USER, key, "read")
    }

    /// Keys and paths only hold `[a-z0-9-/]`: nothing to escape. SRT carries the credentials
    /// in its stream identifier, `<action>:<path>:<user>:<password>`, as MediaMTX reads it.
    fn urls(&self, path: &str, user: &str, key: &str, srt_action: &str) -> StreamUrls {
        let host = self.url_host();
        StreamUrls {
            rtsp: format!("rtsp://{user}:{key}@{host}:{}/{path}", self.rtsp_port),
            rtmp: format!("rtmp://{host}:{}/{path}?user={user}&pass={key}", self.rtmp_port),
            srt: format!("srt://{host}:{}?streamid={srt_action}:{path}:{user}:{key}", self.srt_port),
        }
    }
}

/// Identifier of the feed of a stream in TAK Server.
pub fn feed_uid(stream_id: Uuid) -> String {
    format!("{FEED_UID_PREFIX}{stream_id}")
}

/// The path in a feed's address (`rtsp://view:…@host:8554/alpha/drone-1` gives
/// `alpha/drone-1`), for the journal; `None` when it has none.
pub fn path_of_url(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let path = rest.split_once('/')?.1;
    let path = path.split(['?', '#']).next().unwrap_or_default();
    (!path.is_empty()).then(|| path.to_string())
}

/// A feed TAK Server must list, and the group it must list it to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesiredFeed {
    pub feed: VideoFeed,
    pub group: String,
    /// Path of the stream on the video server.
    pub path: String,
}

/// What a pass does to TAK Server's video feeds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedAction {
    Add(DesiredFeed),
    /// Same identifier, another name or address (the keys were regenerated).
    Update(DesiredFeed),
    /// A feed of TeamTAK whose stream is gone or not valid any more.
    Remove(VideoFeed),
}

/// Compares the feeds TAK Server lists with those it must list. Only TeamTAK's own feeds
/// ([`FEED_UID_PREFIX`]) are ever removed. The group of a listed feed cannot be read back,
/// and never needs to change: a stream keeps its path, hence its group, for its whole life.
pub fn plan_feeds(desired: &[DesiredFeed], listed: &[VideoFeed]) -> Vec<FeedAction> {
    let mut actions = Vec::new();
    for want in desired {
        match listed.iter().find(|f| f.uid == want.feed.uid) {
            None => actions.push(FeedAction::Add(want.clone())),
            Some(have) if have.alias != want.feed.alias || have.url != want.feed.url => {
                actions.push(FeedAction::Update(want.clone()))
            }
            Some(_) => {}
        }
    }
    for have in listed {
        if have.uid.starts_with(FEED_UID_PREFIX) && !desired.iter().any(|d| d.feed.uid == have.uid) {
            actions.push(FeedAction::Remove(have.clone()));
        }
    }
    actions
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &[u8] = b"0123456789abcdef0123456789abcdef";

    #[test]
    fn keys_are_long_distinct_and_change_with_the_generation() {
        let id = Uuid::new_v4();
        let keys = StreamKeys::derive(KEY, id, 1);
        assert_eq!(keys.publish.len(), 24);
        assert!(keys.publish.bytes().all(|b| KEY_ALPHABET.contains(&b)));
        assert_ne!(keys.publish, keys.view, "watching must not give the right to publish");
        assert_eq!(keys, StreamKeys::derive(KEY, id, 1), "derived again, not stored");
        let regenerated = StreamKeys::derive(KEY, id, 2);
        assert_ne!(regenerated.publish, keys.publish);
        assert_ne!(regenerated.view, keys.view);
        assert_ne!(StreamKeys::derive(KEY, Uuid::new_v4(), 1).publish, keys.publish, "another stream");
        assert_ne!(StreamKeys::derive(b"another application key", id, 1).publish, keys.publish);
    }

    #[test]
    fn who_may_publish_and_watch() {
        let keys = StreamKeys { publish: "p".repeat(24), view: "v".repeat(24) };
        let (p, v) = (keys.publish.clone(), keys.view.clone());
        assert!(may(VideoAction::Publish, "publish", &p, &keys));
        assert!(!may(VideoAction::Publish, "view", &v, &keys), "the view key cannot publish");
        assert!(!may(VideoAction::Publish, "view", &p, &keys), "the key goes with its user");
        assert!(may(VideoAction::Read, "view", &v, &keys));
        assert!(may(VideoAction::Read, "publish", &p, &keys), "the publisher may check the picture");
        assert!(!may(VideoAction::Read, "publish", &v, &keys));
        assert!(!may(VideoAction::Read, "", "", &keys), "no credentials: the server asks for them");
        assert!(!may(VideoAction::Read, "view", &v[..23], &keys));
        for other in ["playback", "api", "metrics", "pprof", ""] {
            assert!(!may(VideoAction::parse(other), "publish", &p, &keys), "{other}");
        }
    }

    #[test]
    fn names_and_paths() {
        assert_eq!(stream_name("Drone 1").as_deref(), Some("drone-1"));
        assert_eq!(stream_name("Caméra d'Élise").as_deref(), Some("camera-d-elise"));
        assert_eq!(stream_name("7").as_deref(), Some("7"));
        assert_eq!(stream_name("!!!"), None);
        assert_eq!(stream_name(&"x".repeat(50)).map(|n| n.len()), Some(32));
        assert_eq!(clean_label("  Drone   1 ").as_deref(), Some("Drone 1"));
        assert_eq!(clean_label("   "), None);
        assert_eq!(clean_label(&"x".repeat(41)), None);
        assert_eq!(clean_label("a\u{7}b"), None);

        assert_eq!(stream_path("alpha", "drone-1"), "alpha/drone-1");
        assert_eq!(split_path("alpha/drone-1"), Some(("alpha", "drone-1")));
        assert_eq!(split_path("/evt-exercise/cam"), Some(("evt-exercise", "cam")));
        for bad in
            ["alpha", "alpha/", "/drone", "alpha/drone/1", "alpha/Drone", "a/drone", "anon/drone", "../x", "alpha/-x", "evt-/x"]
        {
            assert_eq!(split_path(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn addresses() {
        let at = VideoEndpoints { host: "tak.example.org".into(), rtsp_port: 8554, rtmp_port: 1935, srt_port: 8890 };
        let publish = at.publish_urls("alpha/drone-1", "pkey");
        assert_eq!(publish.rtsp, "rtsp://publish:pkey@tak.example.org:8554/alpha/drone-1");
        assert_eq!(publish.rtmp, "rtmp://tak.example.org:1935/alpha/drone-1?user=publish&pass=pkey");
        assert_eq!(publish.srt, "srt://tak.example.org:8890?streamid=publish:alpha/drone-1:publish:pkey");
        let view = at.view_urls("alpha/drone-1", "vkey");
        assert_eq!(view.rtsp, "rtsp://view:vkey@tak.example.org:8554/alpha/drone-1");
        assert_eq!(view.srt, "srt://tak.example.org:8890?streamid=read:alpha/drone-1:view:vkey");
        let v6 = VideoEndpoints { host: "::1".into(), ..at };
        assert_eq!(v6.view_urls("alpha/x", "k").rtsp, "rtsp://view:k@[::1]:8554/alpha/x");

        assert_eq!(path_of_url(&view.rtsp).as_deref(), Some("alpha/drone-1"));
        assert_eq!(path_of_url(&publish.rtmp).as_deref(), Some("alpha/drone-1"));
        assert_eq!(path_of_url("rtsp://host:8554"), None);
        assert_eq!(path_of_url("not a url"), None);
    }

    fn feed(uid: &str, url: &str) -> VideoFeed {
        VideoFeed { uid: uid.into(), alias: "Drone 1 · Alpha".into(), url: url.into() }
    }

    fn desired(uid: &str, url: &str) -> DesiredFeed {
        DesiredFeed { feed: feed(uid, url), group: "alpha".into(), path: "alpha/drone-1".into() }
    }

    #[test]
    fn feeds_are_added_updated_and_removed() {
        let ours = format!("{FEED_UID_PREFIX}1");
        let gone = format!("{FEED_UID_PREFIX}2");
        let want = vec![desired(&ours, "rtsp://view:new@h:8554/alpha/drone-1")];
        assert_eq!(plan_feeds(&want, &[]), vec![FeedAction::Add(want[0].clone())]);
        assert_eq!(plan_feeds(&want, &[want[0].feed.clone()]), vec![], "nothing to do");
        let stale = feed(&ours, "rtsp://view:old@h:8554/alpha/drone-1");
        assert_eq!(plan_feeds(&want, &[stale]), vec![FeedAction::Update(want[0].clone())], "keys regenerated");
        let someone_elses = feed("camera-at-the-gate", "rtsp://cam/1");
        let leftover = feed(&gone, "rtsp://view:k@h:8554/alpha/old");
        assert_eq!(
            plan_feeds(&want, &[want[0].feed.clone(), someone_elses, leftover.clone()]),
            vec![FeedAction::Remove(leftover)],
            "only TeamTAK's own feeds are removed"
        );
    }
}
