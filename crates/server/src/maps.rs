//! Maps: the map sources of the administrators' catalogue and of each team, the maps of each
//! team and of each event, and what the reconciliation hands to TAK Server.
//!
//! The rules (what a map source file must hold, how a source is checked, the device profiles
//! TAK Server keeps, the terms of the providers) are in `teamtak_core::maps`. Here:
//!
//! - the catalogue, for the administrators: upload a map source file or a zip of them, add
//!   the suggested sources, check one again, remove one;
//! - the team page: the maps of the team's channel and of the channels of its events, which
//!   the leaders pick from the catalogue and from the team's own sources (they add those
//!   themselves), and a data package of them for devices that do not take TAK Server's
//!   profiles (iTAK);
//! - the checks: every source is checked before it is stored, and again every few hours
//!   ([`run_checks_periodically`]); one that fails twice in a row is held back;
//! - [`desired_profiles`], the device profiles TAK Server must hold at a given time.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::PgPool;
use teamtak_core::events::{clock_now, phase, EventPhase};
use teamtak_core::maps::{
    after_check, build_maps_package, enrollment_profile, expand_upload, maps_profile, parse_map_source, source_file_name, terms_of,
    CheckError, DesiredProfile, MapSource, SourceError, TileChecker, MAX_UPLOAD_BYTES, MAX_UPLOAD_FILES, SUGGESTIONS,
};
use teamtak_core::policy::{can, Action};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::audit::{self, AuditAction, Detail, Details, Target};
use crate::auth::{Account, AdminAccount, CurrentAccount};
use crate::error::{ApiError, ApiResult};
use crate::reconciler::Trigger;
use crate::state::SharedState;
use crate::teams::{apply_change, team_context, Team};

/// Maps one channel may have.
pub const MAX_MAPS_PER_CHANNEL: usize = 30;
/// Map sources the catalogue and the teams may hold together.
const MAX_SOURCES: i64 = 500;
/// Longest a check of one source may take (it tries a few tiles).
const CHECK_BUDGET: Duration = Duration::from_secs(20);
/// Sources of one upload checked at the same time.
const CHECKS_AT_ONCE: usize = 6;
/// How often every source is checked again.
const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 3600);
/// A source checked less than this long ago is not checked again on request.
const RECHECK_AFTER: time::Duration = time::Duration::seconds(30);

fn rfc3339(t: OffsetDateTime) -> String {
    t.format(&Rfc3339).unwrap_or_default()
}

// ------------------------------------------------------------------------------- sources

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SourceRow {
    pub id: Uuid,
    pub file_name: String,
    pub name: String,
    pub url: String,
    pub url_host: String,
    pub min_zoom: Option<i16>,
    pub max_zoom: Option<i16>,
    pub tile_type: Option<String>,
    /// The team that added it; `None` for the administrators' catalogue.
    pub team_id: Option<Uuid>,
    pub team_name: Option<String>,
    pub checked_at: OffsetDateTime,
    pub check_error: Option<String>,
    pub check_status: Option<i32>,
    pub broken: bool,
    pub updated_at: OffsetDateTime,
}

/// Columns of [`SourceRow`], on `map_sources s LEFT JOIN teams o ON o.id = s.team_id`.
macro_rules! source_columns {
    () => {
        "s.id, s.file_name, s.name, s.url, s.url_host, s.min_zoom, s.max_zoom, s.tile_type, s.team_id, o.name AS team_name,
         s.checked_at, s.check_error, s.check_status, s.broken, s.updated_at"
    };
}

macro_rules! source_from {
    () => {
        " FROM map_sources s LEFT JOIN teams o ON o.id = s.team_id"
    };
}

/// A map source as the UI shows it. Its content stays on the server: it is sent to devices.
fn source_view(source: &SourceRow) -> Value {
    let problem = source.check_error.as_ref().map(|code| json!({ "code": code, "status": source.check_status }));
    json!({
        "id": source.id,
        "file_name": source.file_name,
        "name": source.name,
        "url": source.url,
        "host": source.url_host,
        "terms": terms_of(&source.url_host).as_str(),
        "min_zoom": source.min_zoom,
        "max_zoom": source.max_zoom,
        "tile_type": source.tile_type,
        "team_id": source.team_id,
        "team_name": source.team_name,
        "checked_at": rfc3339(source.checked_at),
        // Why the latest check failed, even when the source is not held back yet.
        "problem": problem,
        "broken": source.broken,
        "updated_at": rfc3339(source.updated_at),
    })
}

/// Which sources a list holds.
#[derive(Debug, Clone, Copy)]
enum Scope {
    /// Every source: the administrators' view.
    All,
    /// The catalogue and the sources of one team: what that team picks from.
    Team(Uuid),
    /// The catalogue alone: what an event the administrators organise picks from.
    Catalogue,
}

async fn sources_in(db: &PgPool, scope: Scope) -> sqlx::Result<Vec<SourceRow>> {
    let filter = match scope {
        Scope::All => "",
        Scope::Team(_) => " WHERE s.team_id IS NULL OR s.team_id = $1",
        Scope::Catalogue => " WHERE s.team_id IS NULL",
    };
    let sql = format!(
        concat!("SELECT ", source_columns!(), source_from!(), "{} ORDER BY s.team_id IS NOT NULL, lower(s.name), s.file_name"),
        filter
    );
    let query = sqlx::query_as(sqlx::AssertSqlSafe(sql));
    match scope {
        Scope::Team(team_id) => query.bind(team_id).fetch_all(db).await,
        Scope::All | Scope::Catalogue => query.fetch_all(db).await,
    }
}

async fn source_by_id(db: &PgPool, id: Uuid) -> sqlx::Result<Option<SourceRow>> {
    sqlx::query_as(concat!("SELECT ", source_columns!(), source_from!(), " WHERE s.id = $1")).bind(id).fetch_optional(db).await
}

/// Who a source belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Owner {
    Catalogue,
    Team(Uuid),
}

impl Owner {
    fn team_id(self) -> Option<Uuid> {
        match self {
            Owner::Catalogue => None,
            Owner::Team(id) => Some(id),
        }
    }
}

/// What became of one file of an upload.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Outcome {
    Added,
    Updated,
    /// The same file is there already (in the catalogue, for a team's upload).
    Unchanged,
    Rejected(Problem),
}

/// Why a file was refused: it is not a map source, or it does not work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Problem {
    File(SourceError),
    Check(CheckError),
}

impl Problem {
    fn view(self) -> Value {
        match self {
            Problem::File(e) => json!({ "code": e.code(), "status": null }),
            Problem::Check(e) => json!({ "code": e.code(), "status": e.status() }),
        }
    }
}

impl Outcome {
    fn as_str(&self) -> &'static str {
        match self {
            Outcome::Added => "added",
            Outcome::Updated => "updated",
            Outcome::Unchanged => "unchanged",
            Outcome::Rejected(_) => "rejected",
        }
    }
}

fn rejected(file: &str, problem: Problem) -> (Value, Outcome) {
    (json!({ "file": file, "outcome": "rejected", "problem": problem.view() }), Outcome::Rejected(problem))
}

/// Checks a source, within [`CHECK_BUDGET`].
async fn check(checker: &TileChecker, source: &MapSource) -> Result<(), CheckError> {
    tokio::time::timeout(CHECK_BUDGET, checker.check(source)).await.unwrap_or(Err(CheckError::Unreachable))
}

/// A file name no other owner's source has: the one asked for, or the same with `-2`, `-3`…
async fn free_file_name(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, wanted: &str) -> ApiResult<String> {
    let stem = &wanted[..wanted.len() - 4];
    for n in 1..100 {
        let candidate = if n == 1 { wanted.to_string() } else { format!("{stem}-{n}.xml") };
        let (taken,): (bool,) = sqlx::query_as("SELECT EXISTS (SELECT 1 FROM map_sources WHERE lower(file_name) = lower($1))")
            .bind(&candidate)
            .fetch_one(&mut **tx)
            .await?;
        if !taken {
            return Ok(candidate);
        }
    }
    Err(ApiError::TooManyMapSources)
}

/// Stores one map source file, once it is read and checked. A file name of the same owner is
/// replaced (devices then get the new content in place of the old); a file name taken by
/// another owner gets a suffix, unless a team uploads a file the catalogue already has as it
/// is: the team then uses the catalogue's.
async fn store_source(state: &SharedState, by: &Account, owner: Owner, team: Option<&Team>, uploaded_name: &str, data: &[u8]) -> ApiResult<(Value, Outcome)> {
    let parsed = match parse_map_source(data) {
        Ok(parsed) => parsed,
        Err(e) => return Ok(rejected(uploaded_name, Problem::File(e))),
    };
    if let Err(e) = check(&state.map_checker, &parsed).await {
        tracing::info!(file = uploaded_name, host = %parsed.host, problem = e.code(), "map source refused: it does not work");
        return Ok(rejected(uploaded_name, Problem::Check(e)));
    }
    let wanted = source_file_name(Some(uploaded_name), &parsed.name);
    // Checked as UTF-8 by the parser; a byte-order mark is dropped.
    let content = String::from_utf8_lossy(data).trim_start_matches('\u{feff}').to_string();
    let mut tx = state.db.begin().await?;
    let existing: Option<(Uuid, String, Option<Uuid>)> =
        sqlx::query_as("SELECT id, content, team_id FROM map_sources WHERE lower(file_name) = lower($1) FOR UPDATE")
            .bind(&wanted)
            .fetch_optional(&mut *tx)
            .await?;
    let mine = existing.as_ref().filter(|(_, _, team)| *team == owner.team_id());
    let (id, file_name, outcome) = match (mine, &existing) {
        (Some((id, old, _)), _) if *old == content => (*id, wanted.clone(), Outcome::Unchanged),
        (Some((id, _, _)), _) => {
            sqlx::query(
                "UPDATE map_sources SET name = $2, content = $3, url = $4, url_host = $5, min_zoom = $6, max_zoom = $7,
                        tile_type = $8, checked_at = now(), check_failures = 0, check_error = NULL, check_status = NULL,
                        broken = false, updated_at = now()
                  WHERE id = $1",
            )
            .bind(id)
            .bind(&parsed.name)
            .bind(&content)
            .bind(&parsed.url)
            .bind(&parsed.host)
            .bind(parsed.min_zoom.map(i16::from))
            .bind(parsed.max_zoom.map(i16::from))
            .bind(&parsed.tile_type)
            .execute(&mut *tx)
            .await?;
            (*id, wanted.clone(), Outcome::Updated)
        }
        // A team uploads a file the catalogue has as it is.
        (None, Some((id, old, None))) if owner != Owner::Catalogue && *old == content => (*id, wanted.clone(), Outcome::Unchanged),
        (None, _) => {
            let (count,): (i64,) = sqlx::query_as("SELECT count(*) FROM map_sources").fetch_one(&mut *tx).await?;
            if count >= MAX_SOURCES {
                return Err(ApiError::TooManyMapSources);
            }
            let file_name = free_file_name(&mut tx, &wanted).await?;
            let id = Uuid::new_v4();
            sqlx::query(
                "INSERT INTO map_sources (id, file_name, name, content, url, url_host, min_zoom, max_zoom, tile_type, team_id, created_by)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
            )
            .bind(id)
            .bind(&file_name)
            .bind(&parsed.name)
            .bind(&content)
            .bind(&parsed.url)
            .bind(&parsed.host)
            .bind(parsed.min_zoom.map(i16::from))
            .bind(parsed.max_zoom.map(i16::from))
            .bind(&parsed.tile_type)
            .bind(owner.team_id())
            .bind(by.id)
            .execute(&mut *tx)
            .await?;
            (id, file_name, Outcome::Added)
        }
    };
    tx.commit().await?;
    let action = match outcome {
        Outcome::Added => Some(AuditAction::MapSourceAdded),
        Outcome::Updated => Some(AuditAction::MapSourceUpdated),
        _ => None,
    };
    if let Some(action) = action {
        tracing::info!(file = %file_name, host = %parsed.host, by = %by.email, outcome = outcome.as_str(), "map source stored");
        let target = match team {
            Some(team) => Target::in_team(team, &parsed.name),
            None => Target::object(&parsed.name),
        };
        audit::record(&state.db, audit::Actor::Account(by), action, target, Details::new().text(Detail::File, &file_name)).await;
    }
    let view = json!({
        "file": uploaded_name,
        "outcome": outcome.as_str(),
        "id": id,
        "file_name": file_name,
        "name": parsed.name,
        "host": parsed.host,
        "terms": terms_of(&parsed.host).as_str(),
    });
    Ok((view, outcome))
}

/// Stores the map sources of an upload, checking several at a time.
async fn store_upload(state: &SharedState, by: &Account, owner: Owner, team: Option<&Team>, name: &str, data: &[u8]) -> ApiResult<Vec<(Value, Outcome)>> {
    let files = match expand_upload(name, data) {
        Ok(files) if files.is_empty() => return Ok(vec![rejected(name, Problem::File(SourceError::NotAnUpload))]),
        Ok(files) => files,
        Err(e) => return Ok(vec![rejected(name, Problem::File(e))]),
    };
    let mut results = Vec::with_capacity(files.len());
    for batch in files.chunks(CHECKS_AT_ONCE) {
        let stored = futures_join(batch.iter().map(|file| store_source(state, by, owner, team, &file.name, &file.data))).await;
        for result in stored {
            results.push(result?);
        }
    }
    Ok(results)
}

/// Runs futures at the same time and returns their results in order.
async fn futures_join<F: std::future::Future>(futures: impl Iterator<Item = F>) -> Vec<F::Output> {
    let futures: Vec<_> = futures.map(Box::pin).collect();
    let mut outputs: Vec<Option<F::Output>> = futures.iter().map(|_| None).collect();
    let mut pending: Vec<(usize, std::pin::Pin<Box<F>>)> = futures.into_iter().enumerate().collect();
    std::future::poll_fn(|cx| {
        pending.retain_mut(|(i, future)| match future.as_mut().poll(cx) {
            std::task::Poll::Ready(output) => {
                outputs[*i] = Some(output);
                false
            }
            std::task::Poll::Pending => true,
        });
        if pending.is_empty() {
            std::task::Poll::Ready(())
        } else {
            std::task::Poll::Pending
        }
    })
    .await;
    outputs.into_iter().map(|o| o.expect("every future finished")).collect()
}

#[derive(Deserialize)]
pub struct UploadBody {
    /// The name of the file as chosen (`IGN-Plan.xml`, `maps.zip`).
    name: String,
    /// Its content, base64.
    data: String,
}

fn decode(body: &UploadBody) -> ApiResult<Vec<u8>> {
    base64::engine::general_purpose::STANDARD.decode(body.data.as_bytes()).map_err(|_| ApiError::InvalidInput)
}

/// `GET /api/admin/map-sources`: every source, the catalogue's and the teams', how many teams
/// and events use each, and the suggested sources.
pub async fn admin_catalogue(State(state): State<SharedState>, AdminAccount(account): AdminAccount) -> ApiResult<Json<Value>> {
    account.require_platform(Action::ManageMapCatalogue)?;
    let sources = sources_in(&state.db, Scope::All).await?;
    let uses: Vec<(Uuid, i64, i64)> = sqlx::query_as(
        "SELECT s.id,
                (SELECT count(*) FROM team_maps m WHERE m.source_id = s.id),
                (SELECT count(*) FROM event_maps m JOIN events e ON e.id = m.event_id
                  WHERE m.source_id = s.id AND e.cancelled_at IS NULL AND e.ends_at > now())
           FROM map_sources s",
    )
    .fetch_all(&state.db)
    .await?;
    let uses: BTreeMap<Uuid, (i64, i64)> = uses.into_iter().map(|(id, teams, events)| (id, (teams, events))).collect();
    let present: BTreeSet<String> = sources.iter().filter(|s| s.team_id.is_none()).map(|s| s.file_name.to_lowercase()).collect();
    let views: Vec<Value> = sources
        .iter()
        .map(|s| {
            let (teams, events) = uses.get(&s.id).copied().unwrap_or_default();
            let mut view = source_view(s);
            view["teams"] = json!(teams);
            view["events"] = json!(events);
            view
        })
        .collect();
    let suggestions: Vec<Value> = SUGGESTIONS
        .iter()
        .map(|s| {
            let host = s.url.split('/').nth(2).unwrap_or_default();
            json!({
                "key": s.key,
                "name": s.name,
                "file_name": s.file_name,
                "host": host,
                "terms": terms_of(host).as_str(),
                "present": present.contains(&s.file_name.to_lowercase()),
            })
        })
        .collect();
    Ok(Json(json!({
        "sources": views,
        "suggestions": suggestions,
        "limits": { "upload_bytes": MAX_UPLOAD_BYTES, "files": MAX_UPLOAD_FILES, "per_channel": MAX_MAPS_PER_CHANNEL },
    })))
}

/// `POST /api/admin/map-sources`: one file, a map source or a zip of them (an ATAK data
/// package of map sources works as it is), for the catalogue. Each map source that is read
/// and works is added, or replaces the catalogue's one with the same file name; the others
/// are reported and skipped.
pub async fn upload(
    State(state): State<SharedState>,
    AdminAccount(account): AdminAccount,
    Json(body): Json<UploadBody>,
) -> ApiResult<Json<Value>> {
    account.require_platform(Action::ManageMapCatalogue)?;
    let data = decode(&body)?;
    let results = store_upload(&state, &account, Owner::Catalogue, None, &body.name, &data).await?;
    // A new source is in no channel yet; a replaced one may be in several.
    let replaced = results.iter().any(|(_, outcome)| *outcome == Outcome::Updated);
    let synced = !replaced || apply_change(&state).await;
    Ok(Json(json!({ "results": results.into_iter().map(|(view, _)| view).collect::<Vec<_>>(), "synced": synced })))
}

#[derive(Deserialize)]
pub struct SuggestionsBody {
    keys: Vec<String>,
}

/// `POST /api/admin/map-sources/suggestions`: adds suggested sources to the catalogue.
pub async fn add_suggestions(
    State(state): State<SharedState>,
    AdminAccount(account): AdminAccount,
    Json(body): Json<SuggestionsBody>,
) -> ApiResult<Json<Value>> {
    account.require_platform(Action::ManageMapCatalogue)?;
    let mut results = Vec::new();
    for key in &body.keys {
        let suggestion = SUGGESTIONS.iter().find(|s| s.key == key).ok_or(ApiError::InvalidInput)?;
        let (view, _) = store_source(&state, &account, Owner::Catalogue, None, suggestion.file_name, suggestion.xml().as_bytes()).await?;
        results.push(view);
    }
    Ok(Json(json!({ "results": results })))
}

/// Removes a source from TeamTAK and from every channel. Devices that have it keep it: a
/// profile never removes a file from a device.
async fn delete_source(state: &SharedState, source: &SourceRow, by: &Account) -> ApiResult<bool> {
    sqlx::query("DELETE FROM map_sources WHERE id = $1").bind(source.id).execute(&state.db).await?;
    tracing::info!(file = %source.file_name, by = %by.email, "map source removed");
    let target = match (source.team_id, &source.team_name) {
        (Some(team_id), Some(team_name)) => Target::in_team_named(team_id, team_name, &source.name),
        _ => Target::object(&source.name),
    };
    let details = Details::new().text(Detail::File, &source.file_name);
    audit::record(&state.db, audit::Actor::Account(by), AuditAction::MapSourceRemoved, target, details).await;
    Ok(apply_change(state).await)
}

/// `DELETE /api/admin/map-sources/{id}`: any source, the catalogue's or a team's.
pub async fn remove_source(
    State(state): State<SharedState>,
    AdminAccount(account): AdminAccount,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    account.require_platform(Action::ManageMapCatalogue)?;
    let source = source_by_id(&state.db, id).await?.ok_or(ApiError::NotFound)?;
    let synced = delete_source(&state, &source, &account).await?;
    Ok(Json(json!({ "synced": synced })))
}

/// `POST /api/teams/{id}/map-sources`: a leader adds map sources of the team's own (a file
/// or a zip of them). Those that work go on the team's channel at once; only the team sees
/// them.
pub async fn team_upload(
    State(state): State<SharedState>,
    CurrentAccount(account): CurrentAccount,
    Path(team_id): Path<Uuid>,
    Json(body): Json<UploadBody>,
) -> ApiResult<Json<Value>> {
    let ctx = team_context(&state, team_id, &account).await?;
    ctx.team.require_active()?;
    ctx.require(Action::ChooseTeamMaps)?;
    let data = decode(&body)?;
    let results = store_upload(&state, &account, Owner::Team(team_id), Some(&ctx.team), &body.name, &data).await?;
    let stored: Vec<Uuid> = results
        .iter()
        .filter(|(_, outcome)| !matches!(outcome, Outcome::Rejected(_)))
        .filter_map(|(view, _)| view["id"].as_str().and_then(|id| id.parse().ok()))
        .collect();
    let synced = if stored.is_empty() {
        true
    } else {
        let (count,): (i64,) = sqlx::query_as(
            "SELECT count(*) FROM (SELECT source_id FROM team_maps WHERE team_id = $1 UNION SELECT unnest($2::uuid[])) x",
        )
        .bind(team_id)
        .bind(&stored)
        .fetch_one(&state.db)
        .await?;
        if count > MAX_MAPS_PER_CHANNEL as i64 {
            return Err(ApiError::TooManyMaps);
        }
        sqlx::query("INSERT INTO team_maps (team_id, source_id, added_by) SELECT $1, unnest($2::uuid[]), $3 ON CONFLICT DO NOTHING")
            .bind(team_id)
            .bind(&stored)
            .bind(account.id)
            .execute(&state.db)
            .await?;
        let names = chosen_maps_names(&state.db, Channel::Team(team_id)).await?;
        let details = maps_details(&ctx.team.slug, &names);
        audit::record(&state.db, audit::Actor::Account(&account), AuditAction::TeamMapsChanged, Target::team(&ctx.team), details).await;
        apply_change(&state).await
    };
    Ok(Json(json!({ "results": results.into_iter().map(|(view, _)| view).collect::<Vec<_>>(), "synced": synced })))
}

/// `DELETE /api/teams/{id}/map-sources/{source_id}`: a leader removes one of the team's own
/// sources.
pub async fn team_remove_source(
    State(state): State<SharedState>,
    CurrentAccount(account): CurrentAccount,
    Path((team_id, source_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    let ctx = team_context(&state, team_id, &account).await?;
    ctx.require(Action::ChooseTeamMaps)?;
    let source = source_by_id(&state.db, source_id).await?.filter(|s| s.team_id == Some(team_id)).ok_or(ApiError::NotFound)?;
    let synced = delete_source(&state, &source, &account).await?;
    Ok(Json(json!({ "synced": synced })))
}

/// Records the outcome of a check of a stored source. Returns whether it is held back now,
/// and whether that changed.
async fn record_check(db: &PgPool, id: Uuid, failures: i32, result: Result<(), CheckError>) -> sqlx::Result<(bool, bool)> {
    let (failures, broken) = after_check(failures, result.is_ok());
    let error = result.err();
    let (was,): (bool,) = sqlx::query_as(
        "UPDATE map_sources m SET checked_at = now(), check_failures = $2, check_error = $3, check_status = $4, broken = $5
           FROM (SELECT id, broken FROM map_sources WHERE id = $1 FOR UPDATE) old
          WHERE m.id = old.id
      RETURNING old.broken",
    )
    .bind(id)
    .bind(failures)
    .bind(error.map(CheckError::code))
    .bind(error.and_then(CheckError::status).map(i32::from))
    .bind(broken)
    .fetch_one(db)
    .await?;
    Ok((broken, broken != was))
}

/// Checks one stored source and records it; tells the audit log and the reconciliation when
/// it starts or stops being held back.
/// What a check of a stored source needs.
#[derive(sqlx::FromRow)]
struct Stored {
    content: String,
    name: String,
    file_name: String,
    check_failures: i32,
    team_id: Option<Uuid>,
    team_name: Option<String>,
}

async fn check_stored(state: &SharedState, id: Uuid, by: audit::Actor<'_>) -> ApiResult<bool> {
    let row: Option<Stored> = sqlx::query_as(
        "SELECT s.content, s.name, s.file_name, s.check_failures, s.team_id, o.name AS team_name
           FROM map_sources s LEFT JOIN teams o ON o.id = s.team_id WHERE s.id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?;
    let Stored { content, name, file_name, check_failures: failures, team_id, team_name } = row.ok_or(ApiError::NotFound)?;
    let result = match parse_map_source(content.as_bytes()) {
        Ok(source) => check(&state.map_checker, &source).await,
        Err(_) => Err(CheckError::UnsupportedUrl),
    };
    let (broken, changed) = record_check(&state.db, id, failures, result).await?;
    if changed {
        let action = if broken { AuditAction::MapSourceBroken } else { AuditAction::MapSourceWorking };
        tracing::warn!(file = %file_name, broken, problem = result.err().map(CheckError::code), "map source check changed its state");
        let target = match (team_id, &team_name) {
            (Some(team_id), Some(team_name)) => Target::in_team_named(team_id, team_name, &name),
            _ => Target::object(&name),
        };
        let mut details = Details::new().text(Detail::File, &file_name);
        if let Err(e) = result {
            details = details.text(Detail::Reason, e.code());
        }
        audit::record(&state.db, by, action, target, details).await;
    }
    Ok(changed)
}

/// `POST /api/map-sources/{id}/check`: checks a source again now. An administrator checks
/// any; a leader the catalogue's and the team's own. A source checked moments ago is not
/// checked again.
pub async fn check_now(
    State(state): State<SharedState>,
    CurrentAccount(account): CurrentAccount,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let source = source_by_id(&state.db, id).await?.ok_or(ApiError::NotFound)?;
    let allowed = account.is_admin || {
        let led: Vec<(Uuid,)> = sqlx::query_as("SELECT team_id FROM team_members WHERE account_id = $1 AND role = 'leader'")
            .bind(account.id)
            .fetch_all(&state.db)
            .await?;
        !led.is_empty() && source.team_id.is_none_or(|team| led.iter().any(|(t,)| *t == team))
    };
    if !allowed {
        return Err(ApiError::NotFound);
    }
    if clock_now() - source.checked_at >= RECHECK_AFTER {
        let changed = check_stored(&state, id, audit::Actor::Account(&account)).await?;
        if changed {
            apply_change(&state).await;
        }
    }
    let source = source_by_id(&state.db, id).await?.ok_or(ApiError::NotFound)?;
    Ok(Json(source_view(&source)))
}

/// Checks every source again, every [`CHECK_INTERVAL`], for ever. A source that starts or
/// stops being held back changes the device profiles: a pass follows.
pub async fn run_checks_periodically(state: SharedState) {
    // Not at the very start: TAK Server and the network may still be coming up.
    tokio::time::sleep(Duration::from_secs(120)).await;
    loop {
        let ids: Result<Vec<(Uuid,)>, _> =
            sqlx::query_as("SELECT id FROM map_sources ORDER BY checked_at").fetch_all(&state.db).await;
        match ids {
            Err(e) => tracing::warn!(error = %e, "map sources not checked"),
            Ok(ids) => {
                let mut changed = false;
                for (id,) in ids {
                    match check_stored(&state, id, audit::Actor::System).await {
                        Ok(c) => changed |= c,
                        // Removed meanwhile.
                        Err(ApiError::NotFound) => {}
                        Err(e) => tracing::warn!(source = %id, error = ?e, "map source not checked"),
                    }
                }
                if changed {
                    // The platform changed what the devices must get: not drift.
                    if let Err(e) = state.reconciler.run(Trigger::Change).await {
                        tracing::warn!(error = %format!("{e:#}"), "reconciliation after the map checks failed");
                    }
                }
            }
        }
        tokio::time::sleep(CHECK_INTERVAL).await;
    }
}

/// The checker the platform uses.
pub fn checker() -> anyhow::Result<Arc<TileChecker>> {
    TileChecker::new().map(Arc::new).map_err(anyhow::Error::msg)
}

// ------------------------------------------------------------------------------- the maps of a channel

#[derive(Deserialize)]
pub struct ChannelMapsBody {
    pub source_ids: Vec<Uuid>,
}

/// Where a channel's maps are kept.
#[derive(Debug, Clone, Copy)]
pub enum Channel {
    Team(Uuid),
    Event(Uuid),
}

/// Checks a choice of maps: sources of the catalogue, or of the team `own` when one is given.
/// Returns their names, for the audit log.
async fn chosen_sources(db: &PgPool, ids: &[Uuid], own: Option<Uuid>) -> ApiResult<Vec<String>> {
    let unique: BTreeSet<Uuid> = ids.iter().copied().collect();
    if unique.len() > MAX_MAPS_PER_CHANNEL {
        return Err(ApiError::TooManyMaps);
    }
    let ids: Vec<Uuid> = unique.into_iter().collect();
    let names: Vec<(String,)> = sqlx::query_as(
        "SELECT name FROM map_sources WHERE id = ANY($1) AND (team_id IS NULL OR team_id = $2) ORDER BY lower(name)",
    )
    .bind(&ids)
    .bind(own)
    .fetch_all(db)
    .await?;
    if names.len() != ids.len() {
        // Removed meanwhile, or another team's: it does not exist for this one.
        return Err(ApiError::NotFound);
    }
    Ok(names.into_iter().map(|(n,)| n).collect())
}

/// Replaces the maps of a channel and applies the change. `own` is the team whose sources may
/// be picked besides the catalogue's: the team of a team's channel, the host of an event.
/// Returns the names of the maps and whether TAK Server was told.
pub async fn replace_maps(state: &SharedState, channel: Channel, own: Option<Uuid>, ids: &[Uuid], by: &Account) -> ApiResult<(Vec<String>, bool)> {
    let names = chosen_sources(&state.db, ids, own).await?;
    let ids: Vec<Uuid> = ids.iter().copied().collect::<BTreeSet<_>>().into_iter().collect();
    let mut tx = state.db.begin().await?;
    let (table, column, key) = match channel {
        Channel::Team(team_id) => ("team_maps", "team_id", team_id),
        Channel::Event(event_id) => ("event_maps", "event_id", event_id),
    };
    let delete = format!("DELETE FROM {table} WHERE {column} = $1 AND NOT (source_id = ANY($2))");
    sqlx::query(sqlx::AssertSqlSafe(delete)).bind(key).bind(&ids).execute(&mut *tx).await?;
    let insert = format!("INSERT INTO {table} ({column}, source_id, added_by) SELECT $1, unnest($2::uuid[]), $3 ON CONFLICT DO NOTHING");
    sqlx::query(sqlx::AssertSqlSafe(insert)).bind(key).bind(&ids).bind(by.id).execute(&mut *tx).await?;
    tx.commit().await?;
    let synced = apply_change(state).await;
    Ok((names, synced))
}

/// What an audit entry about a channel's maps says: the channel and the names of its maps.
pub fn maps_details(group: &str, names: &[String]) -> Details {
    Details::new().text(Detail::Channel, group).text(Detail::Maps, &names.join(", "))
}

/// The maps of a channel, as the UI lists them.
async fn maps_of(db: &PgPool, channel: Channel) -> sqlx::Result<Vec<SourceRow>> {
    let (table, column, key) = match channel {
        Channel::Team(team_id) => ("team_maps", "team_id", team_id),
        Channel::Event(event_id) => ("event_maps", "event_id", event_id),
    };
    let sql = format!(
        concat!("SELECT ", source_columns!(), source_from!(), " JOIN {} m ON m.source_id = s.id WHERE m.{} = $1 ORDER BY lower(s.name)"),
        table, column
    );
    sqlx::query_as(sqlx::AssertSqlSafe(sql)).bind(key).fetch_all(db).await
}

async fn chosen_maps_names(db: &PgPool, channel: Channel) -> sqlx::Result<Vec<String>> {
    Ok(maps_of(db, channel).await?.into_iter().map(|s| s.name).collect())
}

/// The events of a team that are not over and that it takes part in (hosts or accepted).
#[derive(Debug, sqlx::FromRow)]
struct TeamEvent {
    id: Uuid,
    name: String,
    group_name: String,
    host_team_id: Option<Uuid>,
    starts_at: OffsetDateTime,
    ends_at: OffsetDateTime,
    cancelled_at: Option<OffsetDateTime>,
}

impl TeamEvent {
    fn phase(&self, now: OffsetDateTime) -> EventPhase {
        phase(self.starts_at, self.ends_at, self.cancelled_at, now)
    }
}

async fn open_events_of(db: &PgPool, team_id: Uuid, now: OffsetDateTime) -> sqlx::Result<Vec<TeamEvent>> {
    let rows: Vec<TeamEvent> = sqlx::query_as(
        "SELECT e.id, e.name, e.group_name, e.host_team_id, e.starts_at, e.ends_at, e.cancelled_at
           FROM events e LEFT JOIN event_teams p ON p.event_id = e.id AND p.team_id = $1
          WHERE (e.host_team_id = $1 OR p.status = 'accepted') AND e.cancelled_at IS NULL AND e.ends_at > $2
          ORDER BY e.starts_at, e.id",
    )
    .bind(team_id)
    .bind(now)
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().filter(|e| e.phase(now).is_open()).collect())
}

/// `GET /api/teams/{id}/maps`: the maps of the team's channel and of the channels of the
/// events it takes part in that are not over, who may change them, and what to pick from
/// for whoever may: the catalogue and the team's own sources.
pub async fn team_maps(
    State(state): State<SharedState>,
    CurrentAccount(account): CurrentAccount,
    Path(team_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let ctx = team_context(&state, team_id, &account).await?;
    ctx.require(Action::ViewTeamMaps)?;
    let now = clock_now();
    let active = ctx.team.require_active().is_ok();
    let leads = active && can(ctx.actor, Action::ChooseTeamMaps);
    let mut channels = vec![json!({
        "group": ctx.team.slug,
        "kind": "team",
        "name": ctx.team.name,
        "event_id": null,
        "phase": null,
        "maps": maps_of(&state.db, Channel::Team(team_id)).await?.iter().map(source_view).collect::<Vec<_>>(),
        "can_edit": leads,
    })];
    for event in open_events_of(&state.db, team_id, now).await? {
        // The maps of an event are its organisers': here, the leaders of the host team.
        let can_edit = active && event.host_team_id == Some(team_id) && can(ctx.actor, Action::ManageEvent);
        channels.push(json!({
            "group": event.group_name,
            "kind": "event",
            "name": event.name,
            "event_id": event.id,
            "phase": event.phase(now).as_str(),
            "maps": maps_of(&state.db, Channel::Event(event.id)).await?.iter().map(source_view).collect::<Vec<_>>(),
            "can_edit": can_edit,
        }));
    }
    let editable = channels.iter().any(|c| c["can_edit"] == true);
    let catalogue: Vec<Value> =
        if editable { sources_in(&state.db, Scope::Team(team_id)).await?.iter().map(source_view).collect() } else { Vec::new() };
    Ok(Json(json!({
        "channels": channels,
        "catalogue": catalogue,
        "per_channel": MAX_MAPS_PER_CHANNEL,
        "permissions": { "add_sources": leads },
        "limits": { "upload_bytes": MAX_UPLOAD_BYTES, "files": MAX_UPLOAD_FILES },
    })))
}

/// `PUT /api/teams/{id}/maps`: the maps of the team's channel, from the catalogue and the
/// team's own sources.
pub async fn set_team_maps(
    State(state): State<SharedState>,
    CurrentAccount(account): CurrentAccount,
    Path(team_id): Path<Uuid>,
    Json(body): Json<ChannelMapsBody>,
) -> ApiResult<Json<Value>> {
    let ctx = team_context(&state, team_id, &account).await?;
    ctx.team.require_active()?;
    ctx.require(Action::ChooseTeamMaps)?;
    let (names, synced) = replace_maps(&state, Channel::Team(team_id), Some(team_id), &body.source_ids, &account).await?;
    tracing::info!(team = %ctx.team.slug, maps = names.len(), by = %account.email, "team maps changed");
    let details = maps_details(&ctx.team.slug, &names);
    audit::record(&state.db, audit::Actor::Account(&account), AuditAction::TeamMapsChanged, Target::team(&ctx.team), details).await;
    Ok(Json(json!({ "synced": synced })))
}

/// The maps of an event, for its organisers' page: the chosen sources and what to pick from
/// (the catalogue, and the host team's own sources for an event a team hosts).
pub async fn event_maps_view(state: &SharedState, event_id: Uuid, host: Option<Uuid>) -> ApiResult<Value> {
    let maps = maps_of(&state.db, Channel::Event(event_id)).await?;
    let scope = host.map_or(Scope::Catalogue, Scope::Team);
    let catalogue: Vec<Value> = sources_in(&state.db, scope).await?.iter().map(source_view).collect();
    Ok(json!({
        "maps": maps.iter().map(source_view).collect::<Vec<_>>(),
        "catalogue": catalogue,
        "per_channel": MAX_MAPS_PER_CHANNEL,
    }))
}

/// `GET /api/teams/{id}/maps/package`: a data package of the maps the team's devices get now
/// (the team's channel and the events running, sources that work), to import by hand where
/// TAK Server's profiles do not reach (iTAK, or ATAK with "Apply TAK Server profile updates"
/// off).
pub async fn team_maps_package(
    State(state): State<SharedState>,
    CurrentAccount(account): CurrentAccount,
    Path(team_id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = team_context(&state, team_id, &account).await?;
    ctx.team.require_active()?;
    ctx.require(Action::ViewTeamMaps)?;
    let now = clock_now();
    let events: Vec<Uuid> =
        open_events_of(&state.db, team_id, now).await?.into_iter().filter(|e| e.phase(now) == EventPhase::Running).map(|e| e.id).collect();
    let files: Vec<(String, String)> = sqlx::query_as(
        "SELECT DISTINCT s.file_name, s.content FROM map_sources s
          WHERE NOT s.broken
            AND (s.id IN (SELECT source_id FROM team_maps WHERE team_id = $1)
                 OR s.id IN (SELECT source_id FROM event_maps WHERE event_id = ANY($2)))
          ORDER BY s.file_name",
    )
    .bind(team_id)
    .bind(&events)
    .fetch_all(&state.db)
    .await?;
    if files.is_empty() {
        return Err(ApiError::NotFound);
    }
    let files: Vec<(String, Vec<u8>)> = files.into_iter().map(|(name, content)| (name, content.into_bytes())).collect();
    let zip = build_maps_package(&format!("{} maps", ctx.team.name), &files).map_err(|e| {
        tracing::error!(error = %e, "maps package not built");
        ApiError::Internal
    })?;
    let file_name = format!("{}-maps.zip", ctx.team.slug);
    Ok((
        [
            (CONTENT_TYPE, "application/zip".to_string()),
            (CONTENT_DISPOSITION, format!("attachment; filename=\"{file_name}\"")),
            (CACHE_CONTROL, "no-store".to_string()),
        ],
        zip,
    )
        .into_response())
}

// ------------------------------------------------------------------------------- the reconciliation

/// The device profiles TAK Server must hold at `now`: the enrollment profile for every active
/// team, and the maps of each group that has some and exists now (an active team; a running
/// event). A source held back because it does not work is left out.
pub async fn desired_profiles(db: &PgPool, now: OffsetDateTime) -> sqlx::Result<Vec<DesiredProfile>> {
    let teams: Vec<(String,)> = sqlx::query_as("SELECT slug FROM teams WHERE status = 'active'").fetch_all(db).await?;
    let mut maps: BTreeMap<String, BTreeMap<String, Vec<u8>>> = BTreeMap::new();
    let team_maps: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT t.slug, s.file_name, s.content
           FROM team_maps m JOIN teams t ON t.id = m.team_id JOIN map_sources s ON s.id = m.source_id
          WHERE t.status = 'active' AND NOT s.broken",
    )
    .fetch_all(db)
    .await?;
    for (group, file_name, content) in team_maps {
        maps.entry(group).or_default().insert(file_name, content.into_bytes());
    }
    let event_maps: Vec<(String, OffsetDateTime, OffsetDateTime, Option<OffsetDateTime>, String, String)> = sqlx::query_as(
        "SELECT e.group_name, e.starts_at, e.ends_at, e.cancelled_at, s.file_name, s.content
           FROM event_maps m JOIN events e ON e.id = m.event_id JOIN map_sources s ON s.id = m.source_id
          WHERE e.cancelled_at IS NULL AND e.ends_at > $1 AND NOT s.broken",
    )
    .bind(now)
    .fetch_all(db)
    .await?;
    for (group, starts_at, ends_at, cancelled_at, file_name, content) in event_maps {
        if phase(starts_at, ends_at, cancelled_at, now) == EventPhase::Running {
            maps.entry(group).or_default().insert(file_name, content.into_bytes());
        }
    }
    let mut profiles: Vec<DesiredProfile> = enrollment_profile(teams.into_iter().map(|(slug,)| slug).collect()).into_iter().collect();
    profiles.extend(maps.into_iter().filter_map(|(group, files)| maps_profile(&group, files)));
    Ok(profiles)
}

#[cfg(test)]
mod tests {
    use super::*;
    use teamtak_core::maps::Terms;

    #[test]
    fn every_problem_outcome_and_provider_note_is_translated_in_every_language() {
        let locales = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/src/locales");
        let problems: Vec<&str> =
            SourceError::ALL.iter().map(|e| e.code()).chain(CheckError::ALL.iter().map(|e| e.code())).collect();
        for language in crate::locale::languages() {
            let path = locales.join(language).join("translation.json");
            let ui: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            for code in &problems {
                assert!(ui.pointer(&format!("/maps/problem/{code}")).is_some(), "{language}: maps.problem.{code}");
            }
            for terms in Terms::ALL {
                assert!(ui.pointer(&format!("/maps/terms/{}", terms.as_str())).is_some(), "{language}: maps.terms.{}", terms.as_str());
            }
            for outcome in ["added", "updated", "unchanged", "rejected"] {
                assert!(ui.pointer(&format!("/maps/outcome/{outcome}")).is_some(), "{language}: maps.outcome.{outcome}");
            }
            for s in SUGGESTIONS {
                assert!(ui.pointer(&format!("/maps/suggestion/{}", s.key)).is_some(), "{language}: maps.suggestion.{}", s.key);
            }
        }
    }

    #[tokio::test]
    async fn futures_run_together_and_answer_in_order() {
        let started = std::time::Instant::now();
        let delays = [30u64, 10, 20];
        let outputs = futures_join(delays.iter().map(|ms| async move {
            tokio::time::sleep(Duration::from_millis(*ms)).await;
            *ms
        }))
        .await;
        assert_eq!(outputs, delays);
        assert!(started.elapsed() < Duration::from_millis(55), "not one after the other");
    }
}
