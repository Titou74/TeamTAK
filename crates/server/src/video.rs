//! Video streams: declared by teams, carried by the video server (MediaMTX), listed by TAK
//! Server to the members of their group.
//!
//! A team declares a stream for its own channel or for the channel of an event it takes part
//! in; the rules (paths, keys, who may publish or watch) are in `teamtak_core::video`. Here:
//!
//! - the API of the team page: list, declare, show the addresses, regenerate, delete;
//! - [`authorize`], which the video server asks about every device that connects, on the
//!   platform's internal port (nothing outside the stack reaches it);
//! - what the reconciliation needs: the feeds TAK Server must list ([`desired_feeds`]), and
//!   the devices to disconnect from streams that are no longer valid ([`stop_invalid`]);
//! - [`VideoServer`], the client of the video server's control API.

use std::collections::BTreeMap;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgPool;
use teamtak_core::events::{clock_now, phase, EventPhase};
use teamtak_core::policy::{can, Action, VideoStreamRef};
use teamtak_core::reconcile::Change;
use teamtak_core::tak::VideoFeed;
use teamtak_core::video::{
    clean_label, feed_uid, may, split_path, stream_name, stream_path, DesiredFeed, StreamKeys, VideoAction, VideoEndpoints,
};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::audit::{self, AuditAction, Detail, Details, Target};
use crate::auth::{Account, CurrentAccount};
use crate::config::VideoConfig;
use crate::error::{is_unique_violation, ApiError, ApiResult};
use crate::state::SharedState;
use crate::teams::{apply_change, team_context, TeamContext};

/// Streams a team may have in one channel.
const MAX_STREAMS_PER_CHANNEL: i64 = 20;

fn rfc3339(t: OffsetDateTime) -> String {
    t.format(&Rfc3339).unwrap_or_default()
}

// ------------------------------------------------------------------------------- the video server

/// A client or a device connected to the video server, as its control API names it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Peer {
    #[serde(rename = "type")]
    pub kind: String,
    pub id: String,
}

impl Peer {
    /// The protocol, as the UI shows it.
    pub fn protocol(&self) -> &'static str {
        match self.kind.as_str() {
            "rtspSession" => "rtsp",
            "rtspsSession" => "rtsps",
            "rtmpConn" => "rtmp",
            "rtmpsConn" => "rtmps",
            "srtConn" => "srt",
            "webRTCSession" => "webrtc",
            _ => "other",
        }
    }

    /// The part of the control API that disconnects it.
    fn kick_route(&self) -> Option<&'static str> {
        Some(match self.kind.as_str() {
            "rtspSession" => "rtspsessions",
            "rtspsSession" => "rtspssessions",
            "rtmpConn" => "rtmpconns",
            "rtmpsConn" => "rtmpsconns",
            "srtConn" => "srtconns",
            "webRTCSession" => "webrtcsessions",
            _ => return None,
        })
    }
}

/// A path of the video server that someone publishes to or watches.
#[derive(Debug, Clone, Deserialize)]
pub struct LivePath {
    pub name: String,
    /// True while a device publishes to it.
    #[serde(default)]
    pub ready: bool,
    #[serde(default, rename = "readyTime")]
    pub ready_time: Option<String>,
    #[serde(default)]
    pub source: Option<Peer>,
    #[serde(default)]
    pub readers: Vec<Peer>,
    #[serde(default)]
    pub tracks: Vec<String>,
}

impl LivePath {
    /// What the team page shows of a stream being published.
    fn view(&self) -> Value {
        json!({
            "ready": self.ready,
            "since": self.ready_time,
            "readers": self.readers.len(),
            "tracks": self.tracks,
            "protocol": self.source.as_ref().map(Peer::protocol),
        })
    }
}

#[derive(Deserialize)]
struct PathList {
    #[serde(default)]
    items: Vec<LivePath>,
}

/// The video server's control API (`/v3/…` of MediaMTX), on the Docker network.
pub struct VideoServer {
    http: reqwest::Client,
    api_url: String,
    pub endpoints: VideoEndpoints,
}

impl VideoServer {
    pub fn new(config: &VideoConfig) -> anyhow::Result<Self> {
        // Plain HTTP on the Docker network; the helper also sets up TLS, which reqwest wants.
        let http = teamtak_core::public_http_client(Duration::from_secs(5)).map_err(anyhow::Error::msg)?;
        Ok(Self { http, api_url: config.api_url.clone(), endpoints: config.endpoints.clone() })
    }

    /// Every path someone publishes to or watches.
    pub async fn live_paths(&self) -> Result<Vec<LivePath>, String> {
        let url = format!("{}/v3/paths/list?itemsPerPage=1000", self.api_url);
        let response = self.http.get(&url).send().await.map_err(|e| format!("the video server does not answer: {e}"))?;
        if !response.status().is_success() {
            return Err(format!("the video server answered {} to GET /v3/paths/list", response.status()));
        }
        let list: PathList = response.json().await.map_err(|e| format!("unexpected answer from the video server: {e}"))?;
        Ok(list.items)
    }

    /// Disconnects whoever publishes to or watches a path. Returns how many were.
    pub async fn stop(&self, path: &LivePath) -> usize {
        let mut stopped = 0;
        for peer in path.source.iter().chain(&path.readers) {
            let Some(route) = peer.kick_route() else { continue };
            let url = format!("{}/v3/{route}/kick/{}", self.api_url, peer.id);
            match self.http.post(&url).send().await {
                Ok(r) if r.status().is_success() => stopped += 1,
                // Gone in the meantime.
                Ok(r) if r.status() == reqwest::StatusCode::NOT_FOUND => {}
                Ok(r) => tracing::warn!(path = %path.name, peer = %peer.id, status = %r.status(), "video: disconnection refused"),
                Err(e) => tracing::warn!(path = %path.name, peer = %peer.id, error = %e, "video: disconnection failed"),
            }
        }
        stopped
    }

    /// Disconnects whoever uses one path, if anyone does.
    async fn stop_path(&self, name: &str) {
        match self.live_paths().await {
            Ok(paths) => {
                for path in paths.iter().filter(|p| p.name == name) {
                    self.stop(path).await;
                }
            }
            Err(e) => tracing::warn!(path = name, error = %e, "video: devices of the stream not disconnected"),
        }
    }
}

// ------------------------------------------------------------------------------- streams

/// A stream, with what decides whether it is valid: its team and, for an event's stream, the
/// event and whether the team takes part.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct StreamRow {
    pub id: Uuid,
    pub team_id: Uuid,
    pub team_name: String,
    pub team_status: String,
    pub event_id: Option<Uuid>,
    pub group_name: String,
    pub name: String,
    pub label: String,
    pub generation: i32,
    pub created_by: Option<Uuid>,
    pub created_at: OffsetDateTime,
    pub regenerated_at: Option<OffsetDateTime>,
    pub event_name: Option<String>,
    pub starts_at: Option<OffsetDateTime>,
    pub ends_at: Option<OffsetDateTime>,
    pub cancelled_at: Option<OffsetDateTime>,
    /// The team hosts the event or accepted to take part.
    pub takes_part: bool,
}

/// Columns of [`StreamRow`]. A macro, because sqlx only accepts SQL known at compile time.
macro_rules! stream_select {
    () => {
        "SELECT s.id, s.team_id, t.name AS team_name, t.status AS team_status, s.event_id, s.group_name, s.name, s.label,
                s.generation, s.created_by, s.created_at, s.regenerated_at,
                e.name AS event_name, e.starts_at, e.ends_at, e.cancelled_at,
                COALESCE(e.host_team_id = s.team_id OR EXISTS (
                    SELECT 1 FROM event_teams p WHERE p.event_id = s.event_id AND p.team_id = s.team_id AND p.status = 'accepted'
                ), false) AS takes_part
           FROM video_streams s JOIN teams t ON t.id = s.team_id LEFT JOIN events e ON e.id = s.event_id"
    };
}

impl StreamRow {
    pub fn path(&self) -> String {
        stream_path(&self.group_name, &self.name)
    }

    fn event_phase(&self, now: OffsetDateTime) -> Option<EventPhase> {
        Some(phase(self.starts_at?, self.ends_at?, self.cancelled_at, now))
    }

    /// The stream's group exists for its team at `now`: the team is active and, for an
    /// event's stream, the event runs and the team takes part. The same rule as the TAK
    /// groups of the team's accounts (`teamtak_core::events::team_groups`).
    pub fn is_valid(&self, now: OffsetDateTime) -> bool {
        self.team_status == "active"
            && match self.event_id {
                None => true,
                Some(_) => self.takes_part && self.event_phase(now) == Some(EventPhase::Running),
            }
    }

    pub fn keys(&self, mac_key: &[u8]) -> StreamKeys {
        StreamKeys::derive(mac_key, self.id, self.generation)
    }

    /// The name devices show: the stream's, then its team's, so that the streams of an
    /// event tell their teams apart.
    pub fn alias(&self) -> String {
        format!("{} · {}", self.label, self.team_name)
    }

    fn as_ref_for(&self, account: &Account) -> VideoStreamRef {
        VideoStreamRef { declared_by_actor: self.created_by == Some(account.id) }
    }
}

/// Every stream whose group exists at `now`.
pub async fn valid_streams(db: &PgPool, now: OffsetDateTime) -> sqlx::Result<Vec<StreamRow>> {
    let rows: Vec<StreamRow> =
        sqlx::query_as(concat!(stream_select!(), " WHERE t.status = 'active' ORDER BY s.created_at")).fetch_all(db).await?;
    Ok(rows.into_iter().filter(|s| s.is_valid(now)).collect())
}

/// The feeds TAK Server must list: one per valid stream, for its group, with the address to
/// watch it from.
pub fn desired_feeds(streams: &[StreamRow], endpoints: &VideoEndpoints, mac_key: &[u8]) -> Vec<DesiredFeed> {
    streams
        .iter()
        .map(|s| {
            let path = s.path();
            let url = endpoints.view_urls(&path, &s.keys(mac_key).view).rtsp;
            DesiredFeed { feed: VideoFeed { uid: feed_uid(s.id), alias: s.alias(), url }, group: s.group_name.clone(), path }
        })
        .collect()
}

/// Disconnects the devices that publish to or watch a path of no valid stream: a stream
/// deleted, an event over, a team suspended. The video server only checks keys when a
/// device connects; this is what cuts those already connected. Returns what was done, for
/// the reconciliation's journal.
pub async fn stop_invalid(video: &VideoServer, valid: &[StreamRow]) -> Vec<Change> {
    let paths = match video.live_paths().await {
        Ok(paths) => paths,
        Err(e) => {
            tracing::warn!(error = %e, "video: the streams in use could not be checked");
            return Vec::new();
        }
    };
    let valid: Vec<String> = valid.iter().map(StreamRow::path).collect();
    let mut changes = Vec::new();
    for path in paths.iter().filter(|p| !valid.contains(&p.name)) {
        if video.stop(path).await > 0 {
            let group = split_path(&path.name).map(|(g, _)| g.to_string()).unwrap_or_default();
            changes.push(Change::VideoStopped { path: path.name.clone(), group });
        }
    }
    changes
}

// ------------------------------------------------------------------------------- the team's API

fn video_of(state: &SharedState) -> ApiResult<&VideoServer> {
    state.video.as_deref().ok_or(ApiError::VideoOff)
}

/// The events of a team that are not over and that it takes part in (hosts or accepted).
#[derive(Debug, sqlx::FromRow)]
struct TeamEvent {
    id: Uuid,
    name: String,
    group_name: String,
    starts_at: OffsetDateTime,
    ends_at: OffsetDateTime,
    cancelled_at: Option<OffsetDateTime>,
}

impl TeamEvent {
    fn phase(&self, now: OffsetDateTime) -> EventPhase {
        phase(self.starts_at, self.ends_at, self.cancelled_at, now)
    }
}

async fn open_events_of(state: &SharedState, team_id: Uuid, now: OffsetDateTime) -> ApiResult<Vec<TeamEvent>> {
    let rows: Vec<TeamEvent> = sqlx::query_as(
        "SELECT e.id, e.name, e.group_name, e.starts_at, e.ends_at, e.cancelled_at
           FROM events e LEFT JOIN event_teams p ON p.event_id = e.id AND p.team_id = $1
          WHERE (e.host_team_id = $1 OR p.status = 'accepted') AND e.cancelled_at IS NULL AND e.ends_at > $2
          ORDER BY e.starts_at, e.id",
    )
    .bind(team_id)
    .bind(now)
    .fetch_all(&state.db)
    .await?;
    Ok(rows.into_iter().filter(|e| e.phase(now).is_open()).collect())
}

/// A stream in a list: no key in it.
fn stream_view(
    stream: &StreamRow,
    ctx: &TeamContext,
    account: &Account,
    live: &BTreeMap<String, LivePath>,
    now: OffsetDateTime,
) -> Value {
    let own = stream.team_id == ctx.team.id;
    let r = stream.as_ref_for(account);
    let path = stream.path();
    json!({
        "id": stream.id,
        "label": stream.label,
        "name": stream.name,
        "path": path,
        "team_name": stream.team_name,
        "own": own,
        "declared_by_me": r.declared_by_actor,
        "created_at": rfc3339(stream.created_at),
        "regenerated_at": stream.regenerated_at.map(rfc3339),
        "valid": stream.is_valid(now),
        "live": live.get(&path).map(LivePath::view),
        // The keys of a stream are its team's: the other teams of an event watch it in ATAK.
        "can_use": own && can(ctx.actor, Action::UseVideoStream),
        "can_manage": own && can(ctx.actor, Action::ManageVideoStream(r)),
    })
}

/// `GET /api/teams/{id}/video`: the streams of the team's channel and of the channels of the
/// events it takes part in that are not over, with what the video server says of them.
pub async fn team_video(
    State(state): State<SharedState>,
    CurrentAccount(account): CurrentAccount,
    Path(team_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let ctx = team_context(&state, team_id, &account).await?;
    let Some(video) = state.video.as_deref() else {
        return Ok(Json(json!({ "enabled": false })));
    };
    let now = clock_now();
    let events = open_events_of(&state, team_id, now).await?;
    let event_ids: Vec<Uuid> = events.iter().map(|e| e.id).collect();
    let streams: Vec<StreamRow> = sqlx::query_as(concat!(
        stream_select!(),
        " WHERE (s.team_id = $1 AND s.event_id IS NULL) OR s.event_id = ANY($2) ORDER BY s.created_at"
    ))
    .bind(team_id)
    .bind(&event_ids)
    .fetch_all(&state.db)
    .await?;
    let (live, reachable) = match video.live_paths().await {
        Ok(paths) => (paths.into_iter().map(|p| (p.name.clone(), p)).collect(), true),
        Err(e) => {
            tracing::warn!(error = %e, "video: live streams not read");
            (BTreeMap::new(), false)
        }
    };
    let active = ctx.team.require_active().is_ok();
    let views = |event_id: Option<Uuid>| -> Vec<Value> {
        streams
            .iter()
            .filter(|s| s.event_id == event_id)
            // Another team's stream is shown while it is valid: it is then in ATAK too.
            .filter(|s| s.team_id == team_id || s.is_valid(now))
            .map(|s| stream_view(s, &ctx, &account, &live, now))
            .collect()
    };
    let mut channels = vec![json!({
        "group": ctx.team.slug,
        "kind": "team",
        "name": ctx.team.name,
        "event_id": null,
        "phase": null,
        "streams": views(None),
    })];
    for event in &events {
        channels.push(json!({
            "group": event.group_name,
            "kind": "event",
            "name": event.name,
            "event_id": event.id,
            "phase": event.phase(now).as_str(),
            "streams": views(Some(event.id)),
        }));
    }
    let endpoints = &video.endpoints;
    Ok(Json(json!({
        "enabled": true,
        "reachable": reachable,
        "server": { "host": endpoints.host, "rtsp_port": endpoints.rtsp_port, "rtmp_port": endpoints.rtmp_port, "srt_port": endpoints.srt_port },
        "channels": channels,
        "permissions": { "create": active && can(ctx.actor, Action::CreateVideoStream) },
    })))
}

#[derive(Deserialize)]
pub struct CreateStreamBody {
    label: String,
    /// The event whose channel the stream is for; none for the team's own channel.
    #[serde(default)]
    event_id: Option<Uuid>,
}

#[derive(Serialize)]
pub struct Created {
    id: Uuid,
    path: String,
    /// False when TAK Server could not be told yet: the periodic pass will.
    synced: bool,
}

/// `POST /api/teams/{id}/video`: declares a stream. Its name comes from its label; its group
/// is the team's, or the event's when one is given, which must not be over and must count
/// the team among its participants.
pub async fn create_stream(
    State(state): State<SharedState>,
    CurrentAccount(account): CurrentAccount,
    Path(team_id): Path<Uuid>,
    Json(body): Json<CreateStreamBody>,
) -> ApiResult<(StatusCode, Json<Created>)> {
    video_of(&state)?;
    let ctx = team_context(&state, team_id, &account).await?;
    ctx.team.require_active()?;
    ctx.require(Action::CreateVideoStream)?;
    let label = clean_label(&body.label).ok_or(ApiError::InvalidInput)?;
    let name = stream_name(&label).ok_or(ApiError::InvalidStreamName)?;
    let group = match body.event_id {
        None => ctx.team.slug.clone(),
        Some(event_id) => {
            let now = clock_now();
            let event = open_events_of(&state, team_id, now).await?.into_iter().find(|e| e.id == event_id);
            match event {
                Some(event) => event.group_name,
                // Over, or the team does not take part: tell which without saying more.
                None => {
                    let (exists,): (bool,) = sqlx::query_as(
                        "SELECT EXISTS (SELECT 1 FROM events e LEFT JOIN event_teams p ON p.event_id = e.id AND p.team_id = $2
                                         WHERE e.id = $1 AND (e.host_team_id = $2 OR p.status = 'accepted'))",
                    )
                    .bind(event_id)
                    .bind(team_id)
                    .fetch_one(&state.db)
                    .await?;
                    return Err(if exists { ApiError::EventClosed } else { ApiError::NotFound });
                }
            }
        }
    };

    let mut tx = state.db.begin().await?;
    let (count,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM video_streams WHERE team_id = $1 AND event_id IS NOT DISTINCT FROM $2")
            .bind(team_id)
            .bind(body.event_id)
            .fetch_one(&mut *tx)
            .await?;
    if count >= MAX_STREAMS_PER_CHANNEL {
        return Err(ApiError::TooManyVideoStreams);
    }
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO video_streams (id, team_id, event_id, group_name, name, label, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(id)
    .bind(team_id)
    .bind(body.event_id)
    .bind(&group)
    .bind(&name)
    .bind(&label)
    .bind(account.id)
    .execute(&mut *tx)
    .await
    .map_err(|e| {
        if is_unique_violation(&e, "video_streams_team_path_idx") || is_unique_violation(&e, "video_streams_event_path_idx") {
            ApiError::StreamNameTaken
        } else {
            e.into()
        }
    })?;
    tx.commit().await?;
    let path = stream_path(&group, &name);
    tracing::info!(team = %ctx.team.slug, path, by = %account.email, "video stream declared");
    let details = Details::new().text(Detail::Channel, &group);
    audit::record(
        &state.db,
        audit::Actor::Account(&account),
        AuditAction::VideoStreamCreated,
        Target::in_team(&ctx.team, &path),
        details,
    )
    .await;
    let synced = apply_change(&state).await;
    Ok((StatusCode::CREATED, Json(Created { id, path, synced })))
}

/// Loads a stream and the caller's rights on it. Only its team's members may see its keys:
/// to anyone else, it does not exist.
async fn stream_context(state: &SharedState, stream_id: Uuid, account: &Account) -> ApiResult<(StreamRow, TeamContext)> {
    let stream: Option<StreamRow> =
        sqlx::query_as(concat!(stream_select!(), " WHERE s.id = $1")).bind(stream_id).fetch_optional(&state.db).await?;
    let stream = stream.ok_or(ApiError::NotFound)?;
    let ctx = team_context(state, stream.team_id, account).await?;
    if !can(ctx.actor, Action::UseVideoStream) {
        return Err(ApiError::NotFound);
    }
    Ok((stream, ctx))
}

/// `GET /api/video-streams/{id}`: the addresses to publish to and to watch from, keys
/// included.
pub async fn stream_detail(
    State(state): State<SharedState>,
    CurrentAccount(account): CurrentAccount,
    Path(stream_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let video = video_of(&state)?;
    let (stream, ctx) = stream_context(&state, stream_id, &account).await?;
    let now = clock_now();
    let keys = stream.keys(state.secrets.mac_key());
    let path = stream.path();
    // The keys are shown: who saw them is recorded, the keys never are.
    let details = Details::new().text(Detail::Channel, &stream.group_name);
    audit::record(
        &state.db,
        audit::Actor::Account(&account),
        AuditAction::VideoStreamKeysViewed,
        Target::in_team(&ctx.team, &path),
        details,
    )
    .await;
    let endpoints = &video.endpoints;
    let live = match video.live_paths().await {
        Ok(paths) => paths.iter().find(|p| p.name == path).map(LivePath::view),
        Err(_) => None,
    };
    Ok(Json(json!({
        "id": stream.id,
        "label": stream.label,
        "name": stream.name,
        "path": path,
        "channel": {
            "group": stream.group_name,
            "kind": if stream.event_id.is_some() { "event" } else { "team" },
            "name": stream.event_name.clone().unwrap_or_else(|| ctx.team.name.clone()),
            "phase": stream.event_phase(now).map(EventPhase::as_str),
        },
        "valid": stream.is_valid(now),
        "live": live,
        "server": { "host": endpoints.host, "rtsp_port": endpoints.rtsp_port, "rtmp_port": endpoints.rtmp_port, "srt_port": endpoints.srt_port },
        "publish": { "user": teamtak_core::video::PUBLISH_USER, "key": keys.publish, "urls": endpoints.publish_urls(&path, &keys.publish) },
        "view": { "user": teamtak_core::video::VIEW_USER, "key": keys.view, "urls": endpoints.view_urls(&path, &keys.view) },
        "created_at": rfc3339(stream.created_at),
        "regenerated_at": stream.regenerated_at.map(rfc3339),
        "can_manage": can(ctx.actor, Action::ManageVideoStream(stream.as_ref_for(&account))),
    })))
}

/// `POST /api/video-streams/{id}/regenerate`: new keys. The devices that use the stream are
/// disconnected and must be given the new addresses; ATAK gets its own from TAK Server.
pub async fn regenerate_stream(
    State(state): State<SharedState>,
    CurrentAccount(account): CurrentAccount,
    Path(stream_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let video = video_of(&state)?;
    let (stream, ctx) = stream_context(&state, stream_id, &account).await?;
    ctx.require(Action::ManageVideoStream(stream.as_ref_for(&account)))?;
    sqlx::query("UPDATE video_streams SET generation = generation + 1, regenerated_at = now() WHERE id = $1")
        .bind(stream.id)
        .execute(&state.db)
        .await?;
    let path = stream.path();
    video.stop_path(&path).await;
    tracing::info!(path, by = %account.email, "video stream keys regenerated");
    let details = Details::new().text(Detail::Channel, &stream.group_name);
    audit::record(
        &state.db,
        audit::Actor::Account(&account),
        AuditAction::VideoStreamRegenerated,
        Target::in_team(&ctx.team, &path),
        details,
    )
    .await;
    let synced = apply_change(&state).await;
    Ok(Json(json!({ "synced": synced })))
}

/// `DELETE /api/video-streams/{id}`: the stream is gone from TAK Server and from the video
/// server, and whoever used it is disconnected.
pub async fn delete_stream(
    State(state): State<SharedState>,
    CurrentAccount(account): CurrentAccount,
    Path(stream_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let video = video_of(&state)?;
    let (stream, ctx) = stream_context(&state, stream_id, &account).await?;
    ctx.require(Action::ManageVideoStream(stream.as_ref_for(&account)))?;
    sqlx::query("DELETE FROM video_streams WHERE id = $1").bind(stream.id).execute(&state.db).await?;
    let path = stream.path();
    video.stop_path(&path).await;
    tracing::info!(path, by = %account.email, "video stream removed");
    let details = Details::new().text(Detail::Channel, &stream.group_name);
    audit::record(
        &state.db,
        audit::Actor::Account(&account),
        AuditAction::VideoStreamRemoved,
        Target::in_team(&ctx.team, &path),
        details,
    )
    .await;
    let synced = apply_change(&state).await;
    Ok(Json(json!({ "synced": synced })))
}

// ------------------------------------------------------------------------------- the video server's questions

/// What the video server sends about a device that wants to publish or watch (MediaMTX's
/// HTTP authentication). The fields it adds besides these are ignored.
#[derive(Debug, Deserialize)]
pub struct AuthRequest {
    #[serde(default)]
    user: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    action: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    protocol: String,
    #[serde(default)]
    ip: String,
}

/// `POST /internal/video/auth`, on the internal port: 200 lets the device in, anything else
/// keeps it out. Without credentials the answer is 401, on which the video server asks the
/// device for them (RTSP does so before sending any).
pub async fn authorize(State(state): State<SharedState>, Json(request): Json<AuthRequest>) -> StatusCode {
    if state.video.is_none() {
        return StatusCode::FORBIDDEN;
    }
    if request.user.is_empty() && request.password.is_empty() {
        return StatusCode::UNAUTHORIZED;
    }
    let action = VideoAction::parse(&request.action);
    let allowed = match split_path(&request.path) {
        None => false,
        Some((group, name)) => {
            let candidates: Result<Vec<StreamRow>, _> =
                sqlx::query_as(concat!(stream_select!(), " WHERE s.group_name = $1 AND s.name = $2"))
                    .bind(group)
                    .bind(name)
                    .fetch_all(&state.db)
                    .await;
            match candidates {
                Ok(rows) => {
                    let now = clock_now();
                    // Several events can have had the same group, one at a time: at most one
                    // of their streams is valid now.
                    rows.iter()
                        .find(|s| s.is_valid(now))
                        .is_some_and(|s| may(action, &request.user, &request.password, &s.keys(state.secrets.mac_key())))
                }
                Err(e) => {
                    tracing::error!(error = %e, "video: authentication not checked");
                    return StatusCode::SERVICE_UNAVAILABLE;
                }
            }
        }
    };
    if allowed {
        tracing::debug!(path = %request.path, action = %request.action, protocol = %request.protocol, "video: allowed");
        StatusCode::OK
    } else {
        tracing::info!(path = %request.path, action = %request.action, protocol = %request.protocol, ip = %request.ip, "video: refused");
        StatusCode::FORBIDDEN
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn stream(
        team_status: &str,
        event: Option<(OffsetDateTime, OffsetDateTime, Option<OffsetDateTime>)>,
        takes_part: bool,
    ) -> StreamRow {
        StreamRow {
            id: Uuid::new_v4(),
            team_id: Uuid::new_v4(),
            team_name: "Alpha".into(),
            team_status: team_status.into(),
            event_id: event.map(|_| Uuid::new_v4()),
            group_name: if event.is_some() { "evt-exercise".into() } else { "alpha".into() },
            name: "drone-1".into(),
            label: "Drone 1".into(),
            generation: 1,
            created_by: None,
            created_at: datetime!(2026-11-21 08:00 UTC),
            regenerated_at: None,
            event_name: event.map(|_| "Exercise".into()),
            starts_at: event.map(|e| e.0),
            ends_at: event.map(|e| e.1),
            cancelled_at: event.and_then(|e| e.2),
            takes_part,
        }
    }

    #[test]
    fn a_stream_is_valid_while_its_group_exists() {
        let now = datetime!(2026-11-21 10:00 UTC);
        let (before, after) = (datetime!(2026-11-21 09:00 UTC), datetime!(2026-11-21 11:00 UTC));
        assert!(stream("active", None, false).is_valid(now), "the team's own channel");
        for status in ["pending", "suspended", "rejected"] {
            assert!(!stream(status, None, false).is_valid(now), "{status}");
        }
        assert!(stream("active", Some((before, after, None)), true).is_valid(now), "a running event the team takes part in");
        assert!(!stream("active", Some((before, after, None)), false).is_valid(now), "the team left the event");
        assert!(!stream("active", Some((after, after, None)), true).is_valid(now), "not started yet");
        assert!(!stream("active", Some((before, now, None)), true).is_valid(now), "the end is exclusive");
        assert!(!stream("active", Some((before, after, Some(before))), true).is_valid(now), "cancelled");
        assert!(!stream("suspended", Some((before, after, None)), true).is_valid(now));
    }

    #[test]
    fn feeds_carry_the_view_address_and_the_team() {
        let s = stream("active", None, false);
        let endpoints = VideoEndpoints { host: "tak.example.org".into(), rtsp_port: 8554, rtmp_port: 1935, srt_port: 8890 };
        let feeds = desired_feeds(std::slice::from_ref(&s), &endpoints, b"key");
        assert_eq!(feeds.len(), 1);
        let feed = &feeds[0];
        assert_eq!((feed.group.as_str(), feed.path.as_str()), ("alpha", "alpha/drone-1"));
        assert_eq!(feed.feed.alias, "Drone 1 · Alpha");
        assert_eq!(feed.feed.uid, feed_uid(s.id));
        let view = s.keys(b"key").view;
        assert_eq!(feed.feed.url, format!("rtsp://view:{view}@tak.example.org:8554/alpha/drone-1"));
        assert!(!feed.feed.url.contains(&s.keys(b"key").publish), "the publish key never goes to TAK Server");
    }

    #[test]
    fn the_client_of_the_video_server_builds() {
        let endpoints = VideoEndpoints { host: "localhost".into(), rtsp_port: 8554, rtmp_port: 1935, srt_port: 8890 };
        let config = VideoConfig { api_url: "http://mediamtx:9997".into(), endpoints };
        assert!(VideoServer::new(&config).is_ok(), "no global TLS provider is installed in this test");
    }

    #[test]
    fn the_video_server_is_read_as_it_answers() {
        // Shape observed on MediaMTX 1.21.4.
        let body = json!({"itemCount": 1, "pageCount": 1, "items": [{"name": "alpha/drone-1", "confName": "all_others", "ready": true,
            "readyTime": "2026-11-21T12:36:21.753872475Z", "available": true, "online": true,
            "source": {"type": "rtspSession", "id": "a02e4539"}, "tracks": ["H264"],
            "readers": [{"type": "srtConn", "id": "51184c1c"}], "inboundBytes": 100687, "bytesReceived": 100687}]});
        let list: PathList = serde_json::from_value(body).unwrap();
        let path = &list.items[0];
        assert!(path.ready);
        assert_eq!(path.view()["protocol"], "rtsp");
        assert_eq!(path.view()["readers"], 1);
        assert_eq!(path.source.as_ref().unwrap().kick_route(), Some("rtspsessions"));
        assert_eq!(path.readers[0].kick_route(), Some("srtconns"));
        assert_eq!(Peer { kind: "hlsMuxer".into(), id: "x".into() }.kick_route(), None);
        let empty: PathList = serde_json::from_value(json!({"itemCount": 0, "pageCount": 0, "items": []})).unwrap();
        assert!(empty.items.is_empty());
    }
}
