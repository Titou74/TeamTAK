//! The audit log: who did what, when, on which team or object, from which address.
//!
//! A table, not log lines. One entry per state-changing action of the platform, per
//! administrator sign-in, per use of the rescue command, and per action of the host agent.
//!
//! **No secret ever goes in.** An entry is built from an action (a fixed list), a team, an
//! object (a TAK username, an e-mail address, an event, a service) and [`Details`], whose
//! keys are a fixed list too ([`Detail`]): there is no way to attach a password, a login
//! code, a passphrase or a certificate to an entry by naming a new field.

use std::collections::BTreeMap;

use axum::extract::{ConnectInfo, Query, Request, State};
use axum::middleware::Next;
use axum::response::Response;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::auth::{client_ip, Account, AdminAccount};
use crate::error::{ApiError, ApiResult};
use crate::state::SharedState;
use crate::teams::Team;

tokio::task_local! {
    /// Address of the client of the request being handled.
    static REQUEST_IP: String;
}

/// Makes the client's address known to everything that handles the request.
pub async fn with_request_ip(ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>, request: Request, next: Next) -> Response {
    let ip = client_ip(request.headers(), peer);
    REQUEST_IP.scope(ip, next.run(request)).await
}

fn request_ip() -> String {
    REQUEST_IP.try_with(Clone::clone).unwrap_or_default()
}

macro_rules! audit_actions {
    ($( $(#[$doc:meta])* $variant:ident => $code:literal ),+ $(,)?) => {
        /// Everything the audit log records. The code is stable: it is stored, filtered on,
        /// and translated by the UI (`audit.actions.<code>`).
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum AuditAction {
            $( $(#[$doc])* $variant ),+
        }

        impl AuditAction {
            pub const ALL: &'static [AuditAction] = &[ $( AuditAction::$variant ),+ ];

            pub fn code(self) -> &'static str {
                match self { $( AuditAction::$variant => $code ),+ }
            }
        }
    };
}

audit_actions! {
    /// An administrator signed in. Other sign-ins are not recorded.
    AdminSignedIn => "admin_signed_in",
    /// `teamtak grant-admin` was run on the server.
    RescueCodeIssued => "rescue_code_issued",
    /// An administrator opened TAK Server's own administration pages (at most one entry an
    /// hour per administrator).
    TakAdminOpened => "tak_admin_opened",
    AdminGranted => "admin_granted",
    AdminRevoked => "admin_revoked",
    /// An administrator created an account for an address and e-mailed it an invitation.
    AccountInvited => "account_invited",
    /// An administrator deleted an account and what is tied to it (right to erasure).
    AccountDeleted => "account_deleted",

    TeamRequested => "team_requested",
    /// A team was created active at once: team approval is off.
    TeamCreated => "team_created",
    TeamApproved => "team_approved",
    TeamRejected => "team_rejected",
    TeamCapChanged => "team_cap_changed",
    TeamSuspended => "team_suspended",
    TeamReactivated => "team_reactivated",
    TeamModeChanged => "team_mode_changed",
    TeamDeleted => "team_deleted",

    MemberInvited => "member_invited",
    InvitationCancelled => "invitation_cancelled",
    InvitationAccepted => "invitation_accepted",
    InvitationDeclined => "invitation_declined",
    MemberRemoved => "member_removed",
    MemberLeft => "member_left",
    MemberRoleChanged => "member_role_changed",

    AccessCreated => "access_created",
    AccessRegenerated => "access_regenerated",
    AccessRemoved => "access_removed",
    /// The username, password and QR codes of an access were shown.
    AccessCredentialsViewed => "access_credentials_viewed",
    /// A connection zip was downloaded.
    AccessPackageDownloaded => "access_package_downloaded",

    EventCreated => "event_created",
    EventTeamInvited => "event_team_invited",
    /// An administrator put a team in an event they organise: it takes part at once.
    EventTeamAssigned => "event_team_assigned",
    EventTeamRemoved => "event_team_removed",
    EventInvitationAccepted => "event_invitation_accepted",
    EventInvitationDeclined => "event_invitation_declined",
    EventLeft => "event_left",
    EventRescheduled => "event_rescheduled",
    EventEndedEarly => "event_ended_early",
    EventCancelled => "event_cancelled",

    /// A video stream was declared, for the team's channel or an event's.
    VideoStreamCreated => "video_stream_created",
    /// The addresses and keys of a video stream were shown.
    VideoStreamKeysViewed => "video_stream_keys_viewed",
    VideoStreamRegenerated => "video_stream_regenerated",
    VideoStreamRemoved => "video_stream_removed",

    /// A map source joined the catalogue (uploaded, or a suggested one added).
    MapSourceAdded => "map_source_added",
    /// A map source of the catalogue got a new content: devices get it in place of the old.
    MapSourceUpdated => "map_source_updated",
    /// A map source left the catalogue, and every channel.
    MapSourceRemoved => "map_source_removed",
    /// The maps of a team's channel changed.
    TeamMapsChanged => "team_maps_changed",
    /// The maps of an event's channel changed.
    EventMapsChanged => "event_maps_changed",
    /// A map source failed its checks twice in a row: it is held back from the devices.
    MapSourceBroken => "map_source_broken",
    /// A map source held back works again.
    MapSourceWorking => "map_source_working",

    /// A member sent a data package to TAK Server for one of the team's channels.
    FileUploaded => "file_uploaded",
    /// A member downloaded a data package or a mission's file through the team page.
    FileDownloaded => "file_downloaded",
    /// A data package was removed from TAK Server.
    FileDeleted => "file_deleted",
    /// A member added a file to a Data Sync mission.
    MissionFileAdded => "mission_file_added",
    /// A file was taken out of a Data Sync mission.
    MissionFileRemoved => "mission_file_removed",

    ReconciliationRequested => "reconciliation_requested",
    SettingsChanged => "settings_changed",

    // What the host agent did, on an administrator's request or on the daily schedule.
    ServiceRestarted => "service_restarted",
    BackupRun => "backup_run",
    LetsEncryptSynced => "letsencrypt_synced",
    LogsRotated => "logs_rotated",
}

/// The only fields an entry's details can carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Detail {
    /// Connection mode of a team.
    Mode,
    /// Role given to a member.
    Role,
    /// Kind of a TAK access: shared, individual, external.
    Kind,
    /// Which zip: `enrollment` or `certificate`.
    Form,
    /// Port of TAK Server written in a zip.
    Port,
    MemberCap,
    /// How many accesses an action removed.
    RemovedAccesses,
    /// How many video streams an action removed.
    RemovedStreams,
    /// Name of the other team an action on an event concerns.
    OtherTeam,
    StartsAt,
    EndsAt,
    /// Why an event was cancelled: `host`, `admin`, `team_deleted`.
    Reason,
    /// `schedule` or `manual`.
    Trigger,
    /// `ok`, `failed`, `unchanged`, `updated`.
    Outcome,
    /// Name of a backup archive.
    File,
    SizeBytes,
    Encrypted,
    /// Names of the settings that changed; never their values.
    Settings,
    /// Number of files a housekeeping run deleted.
    DeletedFiles,
    /// The TAK group, team or event, a video stream or maps belong to.
    Channel,
    /// Names of the maps a channel has after a change.
    Maps,
    /// The Data Sync mission a file belongs to.
    Mission,
}

impl Detail {
    #[cfg_attr(not(test), allow(dead_code))]
    pub const ALL: &'static [Detail] = &[
        Detail::Mode,
        Detail::Role,
        Detail::Kind,
        Detail::Form,
        Detail::Port,
        Detail::MemberCap,
        Detail::RemovedAccesses,
        Detail::RemovedStreams,
        Detail::OtherTeam,
        Detail::StartsAt,
        Detail::EndsAt,
        Detail::Reason,
        Detail::Trigger,
        Detail::Outcome,
        Detail::File,
        Detail::SizeBytes,
        Detail::Encrypted,
        Detail::Settings,
        Detail::DeletedFiles,
        Detail::Channel,
        Detail::Maps,
        Detail::Mission,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Detail::Mode => "mode",
            Detail::Role => "role",
            Detail::Kind => "kind",
            Detail::Form => "form",
            Detail::Port => "port",
            Detail::MemberCap => "member_cap",
            Detail::RemovedAccesses => "removed_accesses",
            Detail::RemovedStreams => "removed_streams",
            Detail::OtherTeam => "other_team",
            Detail::StartsAt => "starts_at",
            Detail::EndsAt => "ends_at",
            Detail::Reason => "reason",
            Detail::Trigger => "trigger",
            Detail::Outcome => "outcome",
            Detail::File => "file",
            Detail::SizeBytes => "size_bytes",
            Detail::Encrypted => "encrypted",
            Detail::Settings => "settings",
            Detail::DeletedFiles => "deleted_files",
            Detail::Channel => "channel",
            Detail::Maps => "maps",
            Detail::Mission => "mission",
        }
    }
}

/// Longest text a detail or an object keeps.
const MAX_TEXT: usize = 200;

fn clip(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).take(MAX_TEXT).collect()
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Details(BTreeMap<&'static str, Value>);

impl Details {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn text(mut self, key: Detail, value: &str) -> Self {
        self.0.insert(key.name(), Value::String(clip(value)));
        self
    }

    pub fn number(mut self, key: Detail, value: i64) -> Self {
        self.0.insert(key.name(), json!(value));
        self
    }

    pub fn flag(mut self, key: Detail, value: bool) -> Self {
        self.0.insert(key.name(), json!(value));
        self
    }

    pub fn time(self, key: Detail, value: OffsetDateTime) -> Self {
        let text = value.format(&time::format_description::well_known::Rfc3339).unwrap_or_default();
        self.text(key, &text)
    }

    pub fn to_json(&self) -> Value {
        json!(self.0)
    }
}

/// What an action was on.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Target {
    team_id: Option<Uuid>,
    team_name: String,
    object: String,
}

impl Target {
    pub fn none() -> Self {
        Self::default()
    }

    pub fn team(team: &Team) -> Self {
        Self { team_id: Some(team.id), team_name: team.name.clone(), object: String::new() }
    }

    pub fn object(object: &str) -> Self {
        Self { object: clip(object), ..Self::default() }
    }

    /// Something inside a team: a TAK username, a member's address, an event.
    pub fn in_team(team: &Team, object: &str) -> Self {
        Self { object: clip(object), ..Self::team(team) }
    }

    pub fn in_team_named(team_id: Uuid, team_name: &str, object: &str) -> Self {
        Self { team_id: Some(team_id), team_name: clip(team_name), object: clip(object) }
    }
}

/// Who an entry is attributed to.
#[derive(Debug, Clone, Copy)]
pub enum Actor<'a> {
    Account(&'a Account),
    /// The platform itself: the daily schedule.
    System,
    /// Someone on the server's command line (`docker compose exec app teamtak …`).
    CommandLine,
}

/// Writes one entry. A failure to write is logged and does not undo the action: the action
/// has already happened, and losing it would be worse than a missing line.
pub async fn record(db: &PgPool, actor: Actor<'_>, action: AuditAction, target: Target, details: Details) {
    let (actor_id, actor_email, ip) = match actor {
        Actor::Account(account) => (Some(account.id), account.email.as_str(), request_ip()),
        Actor::System => (None, "", String::new()),
        Actor::CommandLine => (None, "", "command-line".to_string()),
    };
    let written = sqlx::query(
        "INSERT INTO audit_log (actor_id, actor_email, action, team_id, team_name, object, details, ip)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(actor_id)
    .bind(actor_email)
    .bind(action.code())
    .bind(target.team_id)
    .bind(&target.team_name)
    .bind(&target.object)
    .bind(details.to_json())
    .bind(&ip)
    .execute(db)
    .await;
    if let Err(e) = written {
        tracing::error!(action = action.code(), error = %e, "audit entry not written");
    }
}

// ------------------------------------------------------------------------------- reading

#[derive(Serialize, sqlx::FromRow)]
struct AuditRow {
    id: i64,
    #[serde(with = "time::serde::rfc3339")]
    occurred_at: OffsetDateTime,
    actor_email: String,
    action: String,
    team_id: Option<Uuid>,
    team_name: String,
    object: String,
    details: Value,
    ip: String,
}

#[derive(Deserialize, Default)]
pub struct AuditQuery {
    /// RFC 3339; entries at or after it.
    from: Option<String>,
    /// RFC 3339; entries before it.
    to: Option<String>,
    team_id: Option<Uuid>,
    /// Part of the actor's address.
    actor: Option<String>,
    action: Option<String>,
    page: Option<i64>,
    page_size: Option<i64>,
}

fn parse_time(value: &Option<String>) -> ApiResult<Option<OffsetDateTime>> {
    match value.as_deref().filter(|v| !v.is_empty()) {
        None => Ok(None),
        Some(text) => OffsetDateTime::parse(text, &time::format_description::well_known::Rfc3339).map(Some).map_err(|_| ApiError::InvalidInput),
    }
}

/// Escapes the wildcards of a `LIKE` pattern.
fn like_pattern(part: &str) -> String {
    let escaped: String = part.chars().flat_map(|c| if matches!(c, '%' | '_' | '\\') { vec!['\\', c] } else { vec![c] }).collect();
    format!("%{}%", escaped.to_lowercase())
}

/// `GET /api/admin/audit`: the log, newest first, filtered by period, team, actor and kind.
pub async fn list(State(state): State<SharedState>, AdminAccount(admin): AdminAccount, Query(query): Query<AuditQuery>) -> ApiResult<Json<Value>> {
    admin.require_platform(teamtak_core::policy::Action::ViewMonitoring)?;
    let from = parse_time(&query.from)?;
    let to = parse_time(&query.to)?;
    let page_size = query.page_size.unwrap_or(50).clamp(1, 200);
    let page = query.page.unwrap_or(1).max(1);
    let action = query.action.filter(|a| !a.is_empty());
    if let Some(action) = &action {
        if !AuditAction::ALL.iter().any(|known| known.code() == action) {
            return Err(ApiError::InvalidInput);
        }
    }
    let actor = query.actor.as_deref().map(str::trim).filter(|a| !a.is_empty()).map(like_pattern);

    // A constant: every value of the request is a bind parameter.
    const FILTER: &str = "($1::timestamptz IS NULL OR occurred_at >= $1)
          AND ($2::timestamptz IS NULL OR occurred_at < $2)
          AND ($3::uuid IS NULL OR team_id = $3)
          AND ($4::text IS NULL OR lower(actor_email) LIKE $4)
          AND ($5::text IS NULL OR action = $5)";
    let (total,): (i64,) = sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM audit_log WHERE {FILTER}")))
        .bind(from)
        .bind(to)
        .bind(query.team_id)
        .bind(&actor)
        .bind(&action)
        .fetch_one(&state.db)
        .await?;
    let entries: Vec<AuditRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT id, occurred_at, actor_email, action, team_id, team_name, object, details, ip
           FROM audit_log WHERE {FILTER} ORDER BY occurred_at DESC, id DESC LIMIT $6 OFFSET $7"
    )))
    .bind(from)
    .bind(to)
    .bind(query.team_id)
    .bind(&actor)
    .bind(&action)
    .bind(page_size)
    .bind((page - 1) * page_size)
    .fetch_all(&state.db)
    .await?;
    // Teams that appear in the log, deleted ones included, for the filter.
    let teams: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT DISTINCT ON (team_id) team_id, team_name FROM audit_log WHERE team_id IS NOT NULL ORDER BY team_id, occurred_at DESC",
    )
    .fetch_all(&state.db)
    .await?;
    let mut teams: Vec<Value> = teams.into_iter().map(|(id, name)| json!({ "id": id, "name": name })).collect();
    teams.sort_by_key(|t| t["name"].as_str().unwrap_or_default().to_lowercase());
    let settings = crate::settings::load(&state.db).await?;
    Ok(Json(json!({
        "entries": entries,
        "total": total,
        "page": page,
        "page_size": page_size,
        "actions": AuditAction::ALL.iter().map(|a| a.code()).collect::<Vec<_>>(),
        "teams": teams,
        "retention_days": settings.values.audit_retention_days,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn action_codes_are_unique_snake_case() {
        let codes: BTreeSet<&str> = AuditAction::ALL.iter().map(|a| a.code()).collect();
        assert_eq!(codes.len(), AuditAction::ALL.len());
        for code in codes {
            assert!(code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'), "{code}");
        }
    }

    /// Texts through translation keys: every action has a sentence in every language.
    #[test]
    fn every_action_is_translated_in_every_language() {
        let locales = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/src/locales");
        let mut languages = 0;
        for entry in std::fs::read_dir(&locales).unwrap() {
            let file = entry.unwrap().path().join("translation.json");
            if !file.is_file() {
                continue;
            }
            languages += 1;
            let json: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
            for action in AuditAction::ALL {
                let text = json.pointer(&format!("/audit/actions/{}", action.code())).and_then(|v| v.as_str());
                assert!(text.is_some_and(|t| !t.trim().is_empty()), "audit.actions.{} missing in {}", action.code(), file.display());
            }
            for detail in Detail::ALL {
                let text = json.pointer(&format!("/audit/details/{}", detail.name())).and_then(|v| v.as_str());
                assert!(text.is_some_and(|t| !t.trim().is_empty()), "audit.details.{} missing in {}", detail.name(), file.display());
            }
        }
        assert!(languages >= 2);
    }

    /// The fields an entry can carry are a closed list, and none of them can hold a secret.
    #[test]
    fn no_detail_field_is_made_for_a_secret() {
        let forbidden = ["password", "passphrase", "secret", "token", "code", "key", "p12", "certificate", "cookie", "session", "hash"];
        let names: BTreeSet<&str> = Detail::ALL.iter().map(|d| d.name()).collect();
        assert_eq!(names.len(), Detail::ALL.len());
        for name in names {
            for word in forbidden {
                assert!(!name.contains(word), "detail field {name:?} looks like it holds a {word}");
            }
        }
    }

    #[test]
    fn an_entry_holds_what_was_done_and_nothing_else() {
        // What the handlers record when a zip with certificate is downloaded and when the
        // settings are changed: the secret values involved are not parameters of an entry.
        let download = Details::new().text(Detail::Form, "certificate").text(Detail::Kind, "shared");
        assert_eq!(download.to_json(), json!({ "form": "certificate", "kind": "shared" }));
        let settings = Details::new().text(Detail::Settings, "backup_passphrase, webhook_url");
        assert_eq!(settings.to_json(), json!({ "settings": "backup_passphrase, webhook_url" }), "names of settings, never values");
        let backup = Details::new().text(Detail::Trigger, "manual").text(Detail::Outcome, "ok").number(Detail::SizeBytes, 2453579).flag(Detail::Encrypted, true);
        assert_eq!(backup.to_json(), json!({ "trigger": "manual", "outcome": "ok", "size_bytes": 2453579, "encrypted": true }));
    }

    #[test]
    fn texts_are_bounded_and_single_line() {
        let long = "x".repeat(1000);
        let details = Details::new().text(Detail::OtherTeam, &format!("Alpha\r\n{long}"));
        let stored = details.to_json()["other_team"].as_str().unwrap().to_string();
        assert_eq!(stored.chars().count(), MAX_TEXT);
        assert!(!stored.contains('\n') && !stored.contains('\r'));
        assert_eq!(Target::object("caddy\n").object, "caddy");
    }

    #[test]
    fn the_actor_filter_is_a_plain_substring() {
        assert_eq!(like_pattern("Admin@Example.org"), "%admin@example.org%");
        assert_eq!(like_pattern("100%_x\\"), "%100\\%\\_x\\\\%");
    }

    #[tokio::test]
    async fn the_request_address_is_empty_outside_a_request() {
        assert_eq!(request_ip(), "");
        let seen = REQUEST_IP.scope("203.0.113.7".to_string(), async { request_ip() }).await;
        assert_eq!(seen, "203.0.113.7");
    }
}
