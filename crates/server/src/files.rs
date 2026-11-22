//! Team files: the TAK Server files of a team's channels (data packages of Enterprise Sync,
//! files of Data Sync missions), listed, downloaded, sent and removed from the team page.
//!
//! What a team may see and do is decided in `teamtak_core::files`; TAK Server's calls are behind
//! `teamtak_core::tak::TakFiles`. The platform's certificate sees everything, so every request
//! reads TAK Server again and applies the rules: nothing of a file is shown, sent back or removed
//! unless the team's devices would get it now.

use std::collections::{BTreeMap, BTreeSet};

use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_TYPE};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use teamtak_core::events::{clock_now, phase, EventPhase};
use teamtak_core::files::{
    clean_file_name, is_data_package, may_remove, visible_data_packages, visible_missions, wrap_in_data_package, EnterpriseFile, FileRef,
    Mission, MissionFile, DATA_PACKAGE_KEYWORD, MISSION_FILE_KEYWORD,
};
use teamtak_core::policy::{Action, TeamRole};
use teamtak_core::tak::{FileUpload, TakError};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::audit::{self, AuditAction, Detail, Details, Target};
use crate::auth::{Account, CurrentAccount};
use crate::error::{ApiError, ApiResult};
use crate::settings;
use crate::state::SharedState;
use crate::teams::{team_context, TeamContext};

fn rfc3339(t: Option<OffsetDateTime>) -> Option<String> {
    t.and_then(|t| t.format(&Rfc3339).ok())
}

fn tak_error(e: TakError) -> ApiError {
    tracing::warn!(error = %e, "TAK Server files");
    ApiError::TakUnavailable
}

/// A channel a team sends to: its own, or a running event it takes part in.
#[derive(Debug, Clone)]
struct Channel {
    group: String,
    kind: &'static str,
    name: String,
}

/// The team, the caller, and the TAK groups the team's devices have now.
struct Scope {
    ctx: TeamContext,
    channels: Vec<Channel>,
    groups: BTreeSet<String>,
    /// The TAK usernames of the team's accesses: what its devices send is the team's.
    usernames: BTreeSet<String>,
}

impl Scope {
    fn is_leader(&self) -> bool {
        self.ctx.actor.team_role == Some(TeamRole::Leader)
    }
}

/// Loads the team for a request about its files: an active team, and a caller in it.
async fn scope(state: &SharedState, team_id: Uuid, account: &Account) -> ApiResult<Scope> {
    let ctx = team_context(state, team_id, account).await?;
    ctx.require(Action::UseTeamFiles)?;
    ctx.team.require_active()?;
    let now = clock_now();
    let events: Vec<(String, String, OffsetDateTime, OffsetDateTime, Option<OffsetDateTime>)> = sqlx::query_as(
        "SELECT e.name, e.group_name, e.starts_at, e.ends_at, e.cancelled_at
           FROM events e LEFT JOIN event_teams p ON p.event_id = e.id AND p.team_id = $1
          WHERE (e.host_team_id = $1 OR p.status = 'accepted') AND e.cancelled_at IS NULL AND e.ends_at > $2
          ORDER BY e.starts_at, e.id",
    )
    .bind(team_id)
    .bind(now)
    .fetch_all(&state.db)
    .await?;
    let mut channels = vec![Channel { group: ctx.team.slug.clone(), kind: "team", name: ctx.team.name.clone() }];
    for (name, group, starts_at, ends_at, cancelled_at) in events {
        if phase(starts_at, ends_at, cancelled_at, now) == EventPhase::Running {
            channels.push(Channel { group, kind: "event", name });
        }
    }
    let groups = channels.iter().map(|c| c.group.clone()).collect();
    let usernames: Vec<(String,)> =
        sqlx::query_as("SELECT username FROM tak_accesses WHERE team_id = $1").bind(team_id).fetch_all(&state.db).await?;
    Ok(Scope { ctx, channels, groups, usernames: usernames.into_iter().map(|(u,)| u).collect() })
}

/// An upload made through TeamTAK.
#[derive(Debug, Clone, sqlx::FromRow)]
struct Sent {
    team_id: Uuid,
    account_id: Option<Uuid>,
    hash: String,
    primary_key: Option<i64>,
    mission: Option<String>,
    sender: Option<String>,
}

async fn sent_through_teamtak(state: &SharedState) -> ApiResult<Vec<Sent>> {
    Ok(sqlx::query_as(
        "SELECT f.team_id, f.account_id, f.hash, f.primary_key, f.mission, COALESCE(NULLIF(a.display_name, ''), a.email) AS sender
           FROM team_files f LEFT JOIN accounts a ON a.id = f.account_id",
    )
    .fetch_all(&state.db)
    .await?)
}

/// Who sent a file, as the team page shows it, and how the caller relates to it.
fn sender_of(
    scope: &Scope,
    account: &Account,
    submitter: &str,
    record: Option<&Sent>,
) -> (Value, FileRef) {
    let (sender, by_team, by_caller) = match record {
        Some(sent) => (
            json!({ "kind": "teamtak", "name": sent.sender, "team": sent.team_id == scope.ctx.team.id }),
            sent.team_id == scope.ctx.team.id,
            sent.account_id == Some(account.id),
        ),
        None if scope.usernames.contains(submitter) => (json!({ "kind": "access", "name": submitter, "team": true }), true, false),
        None => (json!({ "kind": "device", "name": submitter, "team": false }), false, false),
    };
    (sender, FileRef { caller_is_leader: scope.is_leader(), sent_by_caller: by_caller, sent_by_team: by_team })
}

/// Everything the team sees now: its data packages and its missions with their files. The
/// files of a locked mission are read too, only to keep them out of the data packages.
struct Seen {
    packages: Vec<EnterpriseFile>,
    missions: Vec<(Mission, Option<Vec<MissionFile>>)>,
}

async fn seen(state: &SharedState, scope: &Scope) -> ApiResult<Seen> {
    let missions = state.tak_files.missions().await.map_err(tak_error)?;
    let mut hidden = BTreeSet::new();
    let mut shown = Vec::new();
    // A locked mission's files stay hidden wherever they are, whoever's they are.
    for mission in missions.iter().filter(|m| m.locked()) {
        let files = state.tak_files.mission_files(&mission.name).await.map_err(tak_error)?;
        hidden.extend(files.into_iter().map(|f| f.hash));
    }
    for mission in visible_missions(&missions, &scope.groups) {
        let files = if mission.locked() { None } else { Some(state.tak_files.mission_files(&mission.name).await.map_err(tak_error)?) };
        shown.push((mission.clone(), files));
    }
    let files = state.tak_files.enterprise_files().await.map_err(tak_error)?;
    let packages = visible_data_packages(&files, &scope.groups, &hidden).into_iter().cloned().collect();
    Ok(Seen { packages, missions: shown })
}

/// `GET /api/teams/{id}/files`: the data packages and the missions of the team's channels.
pub async fn team_files(
    State(state): State<SharedState>,
    CurrentAccount(account): CurrentAccount,
    Path(team_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let scope = scope(&state, team_id, &account).await?;
    let seen = seen(&state, &scope).await?;
    let records = sent_through_teamtak(&state).await?;
    let by_key: BTreeMap<i64, &Sent> = records.iter().filter_map(|r| r.primary_key.map(|k| (k, r))).collect();
    let by_mission: BTreeMap<(&str, &str), &Sent> =
        records.iter().filter_map(|r| r.mission.as_deref().map(|m| ((m, r.hash.as_str()), r))).collect();
    let in_team = |groups: &BTreeSet<String>| -> Vec<String> { groups.iter().filter(|g| scope.groups.contains(*g)).cloned().collect() };

    let mut packages: Vec<&EnterpriseFile> = seen.packages.iter().collect();
    packages.sort_by_key(|f| std::cmp::Reverse(f.submitted_at));
    let packages: Vec<Value> = packages
        .into_iter()
        .map(|f| {
            let (sender, relation) = sender_of(&scope, &account, &f.submitter, by_key.get(&f.primary_key).copied());
            json!({
                "key": f.primary_key,
                "name": f.name,
                "size": f.size,
                "submitted_at": rfc3339(f.submitted_at),
                "channels": in_team(&f.groups),
                "sender": sender,
                "can_delete": may_remove(relation),
            })
        })
        .collect();
    let missions: Vec<Value> = seen
        .missions
        .iter()
        .map(|(mission, files)| {
            let files = files.as_ref().map(|files| {
                files
                    .iter()
                    .map(|f| {
                        let record = by_mission.get(&(mission.name.as_str(), f.hash.as_str())).copied();
                        let (sender, relation) = sender_of(&scope, &account, &f.submitter, record);
                        json!({
                            "hash": f.hash,
                            "name": f.name,
                            "size": f.size,
                            "added_at": rfc3339(f.added_at),
                            "sender": sender,
                            "can_remove": mission.writable && may_remove(relation),
                        })
                    })
                    .collect::<Vec<_>>()
            });
            json!({
                "name": mission.name,
                "description": mission.description,
                "channels": in_team(&mission.groups),
                "locked": mission.locked(),
                "writable": mission.writable,
                "created_at": rfc3339(mission.created_at),
                "files": files,
            })
        })
        .collect();
    let max_mb = settings::load(&state.db).await?.values.file_upload_max_mb;
    Ok(Json(json!({
        "channels": scope.channels.iter().map(|c| json!({ "group": c.group, "kind": c.kind, "name": c.name })).collect::<Vec<_>>(),
        "data_packages": packages,
        "missions": missions,
        "max_bytes": u64::from(max_mb) * 1_000_000,
        "max_mb": max_mb,
    })))
}

/// A file sent back to the browser, as it comes from TAK Server.
fn attachment(name: &str, response: reqwest::Response) -> Response {
    // An ASCII fallback, and the real name for browsers that read it.
    let ascii: String = name.chars().map(|c| if c.is_ascii_graphic() || c == ' ' { c } else { '_' }).collect();
    let encoded: String = name
        .bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect();
    let length = response.content_length();
    let mut response = (
        [
            (CONTENT_TYPE, "application/octet-stream".to_string()),
            (CONTENT_DISPOSITION, format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{encoded}")),
            (CACHE_CONTROL, "no-store".to_string()),
        ],
        Body::from_stream(response.bytes_stream()),
    )
        .into_response();
    if let Some(length) = length {
        response.headers_mut().insert(CONTENT_LENGTH, length.into());
    }
    response
}

/// `GET /api/teams/{id}/files/{key}/content`: downloads a data package the team sees.
pub async fn download(
    State(state): State<SharedState>,
    CurrentAccount(account): CurrentAccount,
    Path((team_id, key)): Path<(Uuid, i64)>,
) -> ApiResult<Response> {
    let scope = scope(&state, team_id, &account).await?;
    let seen = seen(&state, &scope).await?;
    let file = seen.packages.into_iter().find(|f| f.primary_key == key).ok_or(ApiError::NotFound)?;
    let content = state.tak_files.file_content(&file.hash).await.map_err(tak_error)?;
    let details = Details::new().text(Detail::File, &file.name).number(Detail::SizeBytes, file.size);
    audit::record(&state.db, audit::Actor::Account(&account), AuditAction::FileDownloaded, Target::team(&scope.ctx.team), details).await;
    Ok(attachment(&file.name, content))
}

/// `GET /api/teams/{id}/missions/{name}/files/{hash}/content`: downloads a file of a mission
/// the team sees, unless the mission is locked.
pub async fn download_mission_file(
    State(state): State<SharedState>,
    CurrentAccount(account): CurrentAccount,
    Path((team_id, mission, hash)): Path<(Uuid, String, String)>,
) -> ApiResult<Response> {
    let scope = scope(&state, team_id, &account).await?;
    let (mission, files) = visible_mission(&state, &scope, &mission).await?;
    let file = files.into_iter().find(|f| f.hash == hash).ok_or(ApiError::NotFound)?;
    let content = state.tak_files.file_content(&file.hash).await.map_err(tak_error)?;
    let details = Details::new().text(Detail::File, &file.name).text(Detail::Mission, &mission.name).number(Detail::SizeBytes, file.size);
    audit::record(&state.db, audit::Actor::Account(&account), AuditAction::FileDownloaded, Target::team(&scope.ctx.team), details).await;
    Ok(attachment(&file.name, content))
}

/// A mission the team sees, with its files; a locked one answers that it is.
async fn visible_mission(state: &SharedState, scope: &Scope, name: &str) -> ApiResult<(Mission, Vec<MissionFile>)> {
    let missions = state.tak_files.missions().await.map_err(tak_error)?;
    let mission = visible_missions(&missions, &scope.groups).into_iter().find(|m| m.name == name).cloned().ok_or(ApiError::NotFound)?;
    if mission.locked() {
        return Err(ApiError::MissionLocked);
    }
    let files = state.tak_files.mission_files(&mission.name).await.map_err(tak_error)?;
    Ok((mission, files))
}

#[derive(Deserialize)]
pub struct UploadQuery {
    /// The file's name, as picked.
    name: String,
    /// The TAK group to store it for: the team's, or a running event's (data packages only).
    #[serde(default)]
    channel: Option<String>,
}

/// The device name TAK Server shows for what the platform sends for a team.
fn creator_uid(scope: &Scope) -> String {
    format!("teamtak-{}", scope.ctx.team.slug)
}

/// Refuses a file larger than the setting (the body limit already stopped larger ones).
async fn check_size(state: &SharedState, body: &Bytes) -> ApiResult<()> {
    let max = u64::from(settings::load(&state.db).await?.values.file_upload_max_mb) * 1_000_000;
    if body.is_empty() {
        return Err(ApiError::InvalidInput);
    }
    if body.len() as u64 > max {
        return Err(ApiError::FileTooLarge);
    }
    Ok(())
}

/// A rough media type from the extension, for TAK Server's records.
fn mime_type(name: &str) -> &'static str {
    match name.rsplit_once('.').map(|(_, ext)| ext.to_ascii_lowercase()).as_deref() {
        Some("zip") => "application/zip",
        Some("pdf") => "application/pdf",
        Some("kml") => "application/vnd.google-earth.kml+xml",
        Some("kmz") => "application/vnd.google-earth.kmz",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("txt") => "text/plain",
        Some("xml") => "application/xml",
        _ => "application/octet-stream",
    }
}

/// The primary key of an upload just made: TAK Server answers with the hash only.
async fn primary_key_of(state: &SharedState, hash: &str, name: &str, creator: &str) -> Option<i64> {
    let files = state.tak_files.enterprise_files().await.ok()?;
    files.iter().filter(|f| f.hash == hash && f.name == name && f.creator_uid == creator).map(|f| f.primary_key).max()
}

/// `POST /api/teams/{id}/files?name=&channel=`: a member sends a file to one of the team's
/// channels. A data package goes as it is; any other file is wrapped in one, so that ATAK
/// imports it. It is stored for that channel's group only.
pub async fn upload(
    State(state): State<SharedState>,
    CurrentAccount(account): CurrentAccount,
    Path(team_id): Path<Uuid>,
    Query(query): Query<UploadQuery>,
    body: Bytes,
) -> ApiResult<Json<Value>> {
    let scope = scope(&state, team_id, &account).await?;
    check_size(&state, &body).await?;
    let picked = clean_file_name(&query.name).ok_or(ApiError::InvalidInput)?;
    let channel = query.channel.as_deref().unwrap_or(&scope.ctx.team.slug);
    let channel = scope.channels.iter().find(|c| c.group == channel).ok_or(ApiError::InvalidInput)?.clone();
    let (name, data) = if is_data_package(&body) {
        let name = if picked.to_ascii_lowercase().ends_with(".zip") { picked.clone() } else { format!("{picked}.zip") };
        (name, body.to_vec())
    } else {
        let data = body.to_vec();
        let picked_for_zip = picked.clone();
        tokio::task::spawn_blocking(move || wrap_in_data_package(&picked_for_zip, &data))
            .await
            .map_err(|_| ApiError::Internal)?
            .map_err(|e| {
                tracing::error!(error = %e, "data package not built");
                ApiError::Internal
            })?
    };
    let size = data.len() as i64;
    let creator = creator_uid(&scope);
    let groups = vec![channel.group.clone()];
    let upload = FileUpload {
        name: &name,
        mime_type: "application/x-zip-compressed",
        keyword: DATA_PACKAGE_KEYWORD,
        creator_uid: &creator,
        groups: &groups,
        data,
    };
    let hash = state.tak_files.upload_file(upload).await.map_err(tak_error)?;
    let key = primary_key_of(&state, &hash, &name, &creator).await;
    sqlx::query(
        "INSERT INTO team_files (id, team_id, account_id, hash, primary_key, mission, name, size, channel)
         VALUES ($1, $2, $3, $4, $5, NULL, $6, $7, $8)",
    )
    .bind(Uuid::new_v4())
    .bind(team_id)
    .bind(account.id)
    .bind(&hash)
    .bind(key)
    .bind(&name)
    .bind(size)
    .bind(&channel.group)
    .execute(&state.db)
    .await?;
    tracing::info!(team = %scope.ctx.team.slug, channel = %channel.group, name, size, by = %account.email, "file sent to TAK Server");
    let details = Details::new().text(Detail::File, &name).text(Detail::Channel, &channel.group).number(Detail::SizeBytes, size);
    audit::record(&state.db, audit::Actor::Account(&account), AuditAction::FileUploaded, Target::team(&scope.ctx.team), details).await;
    Ok(Json(json!({ "key": key, "name": name, "hash": hash, "wrapped": name != picked })))
}

/// `POST /api/teams/{id}/missions/{name}/files?name=`: a member adds a file to a mission the
/// team sees, unless it is locked or read-only. The file goes as it is, stored for the
/// mission's groups.
pub async fn upload_to_mission(
    State(state): State<SharedState>,
    CurrentAccount(account): CurrentAccount,
    Path((team_id, mission)): Path<(Uuid, String)>,
    Query(query): Query<UploadQuery>,
    body: Bytes,
) -> ApiResult<Json<Value>> {
    let scope = scope(&state, team_id, &account).await?;
    check_size(&state, &body).await?;
    let name = clean_file_name(&query.name).ok_or(ApiError::InvalidInput)?;
    let (mission, _) = visible_mission(&state, &scope, &mission).await?;
    if !mission.writable {
        return Err(ApiError::MissionReadOnly);
    }
    let size = body.len() as i64;
    let creator = creator_uid(&scope);
    let groups: Vec<String> = mission.groups.iter().cloned().collect();
    let upload = FileUpload {
        name: &name,
        mime_type: mime_type(&name),
        keyword: MISSION_FILE_KEYWORD,
        creator_uid: &creator,
        groups: &groups,
        data: body.to_vec(),
    };
    let hash = state.tak_files.upload_file(upload).await.map_err(tak_error)?;
    state.tak_files.add_to_mission(&mission.name, &hash, &creator).await.map_err(tak_error)?;
    let key = primary_key_of(&state, &hash, &name, &creator).await;
    sqlx::query(
        "INSERT INTO team_files (id, team_id, account_id, hash, primary_key, mission, name, size, channel)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $6)",
    )
    .bind(Uuid::new_v4())
    .bind(team_id)
    .bind(account.id)
    .bind(&hash)
    .bind(key)
    .bind(&mission.name)
    .bind(&name)
    .bind(size)
    .execute(&state.db)
    .await?;
    tracing::info!(team = %scope.ctx.team.slug, mission = %mission.name, name, size, by = %account.email, "file added to a mission");
    let details = Details::new().text(Detail::File, &name).text(Detail::Mission, &mission.name).number(Detail::SizeBytes, size);
    audit::record(&state.db, audit::Actor::Account(&account), AuditAction::MissionFileAdded, Target::team(&scope.ctx.team), details).await;
    Ok(Json(json!({ "hash": hash, "name": name })))
}

/// The record of a TeamTAK upload, by its Enterprise Sync key or by mission and hash.
async fn record_of(state: &SharedState, key: Option<i64>, mission: Option<(&str, &str)>) -> ApiResult<Option<Sent>> {
    let records = sent_through_teamtak(state).await?;
    Ok(records.into_iter().find(|r| match (key, mission) {
        (Some(key), _) => r.primary_key == Some(key),
        (None, Some((m, h))) => r.mission.as_deref() == Some(m) && r.hash == h,
        _ => false,
    }))
}

/// `DELETE /api/teams/{id}/files/{key}`: removes a data package from TAK Server. A member
/// removes what they sent through TeamTAK, a leader what the team sent.
pub async fn delete(
    State(state): State<SharedState>,
    CurrentAccount(account): CurrentAccount,
    Path((team_id, key)): Path<(Uuid, i64)>,
) -> ApiResult<Json<Value>> {
    let scope = scope(&state, team_id, &account).await?;
    let seen = seen(&state, &scope).await?;
    let file = seen.packages.into_iter().find(|f| f.primary_key == key).ok_or(ApiError::NotFound)?;
    let record = record_of(&state, Some(key), None).await?;
    let (_, relation) = sender_of(&scope, &account, &file.submitter, record.as_ref());
    if !may_remove(relation) {
        return Err(ApiError::Forbidden);
    }
    state.tak_files.delete_file(key).await.map_err(tak_error)?;
    sqlx::query("DELETE FROM team_files WHERE primary_key = $1 AND mission IS NULL").bind(key).execute(&state.db).await?;
    tracing::info!(team = %scope.ctx.team.slug, name = %file.name, by = %account.email, "file removed from TAK Server");
    let details = Details::new().text(Detail::File, &file.name).number(Detail::SizeBytes, file.size);
    audit::record(&state.db, audit::Actor::Account(&account), AuditAction::FileDeleted, Target::team(&scope.ctx.team), details).await;
    Ok(Json(json!({ "deleted": true })))
}

/// `DELETE /api/teams/{id}/missions/{name}/files/{hash}`: takes a file out of a mission (TAK
/// Server keeps it in Enterprise Sync). Same rule as for data packages, on a mission that can
/// be written to.
pub async fn remove_from_mission(
    State(state): State<SharedState>,
    CurrentAccount(account): CurrentAccount,
    Path((team_id, mission, hash)): Path<(Uuid, String, String)>,
) -> ApiResult<Json<Value>> {
    let scope = scope(&state, team_id, &account).await?;
    let (mission, files) = visible_mission(&state, &scope, &mission).await?;
    if !mission.writable {
        return Err(ApiError::MissionReadOnly);
    }
    let file = files.into_iter().find(|f| f.hash == hash).ok_or(ApiError::NotFound)?;
    let record = record_of(&state, None, Some((&mission.name, &hash))).await?;
    let (_, relation) = sender_of(&scope, &account, &file.submitter, record.as_ref());
    if !may_remove(relation) {
        return Err(ApiError::Forbidden);
    }
    state.tak_files.remove_from_mission(&mission.name, &hash, &creator_uid(&scope)).await.map_err(tak_error)?;
    sqlx::query("DELETE FROM team_files WHERE mission = $1 AND hash = $2").bind(&mission.name).bind(&hash).execute(&state.db).await?;
    tracing::info!(team = %scope.ctx.team.slug, mission = %mission.name, name = %file.name, by = %account.email, "file taken out of a mission");
    let details = Details::new().text(Detail::File, &file.name).text(Detail::Mission, &mission.name);
    audit::record(&state.db, audit::Actor::Account(&account), AuditAction::MissionFileRemoved, Target::team(&scope.ctx.team), details).await;
    Ok(Json(json!({ "removed": true })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_types_follow_the_extension() {
        assert_eq!(mime_type("plan.PDF"), "application/pdf");
        assert_eq!(mime_type("zone.kml"), "application/vnd.google-earth.kml+xml");
        assert_eq!(mime_type("noext"), "application/octet-stream");
    }
}
