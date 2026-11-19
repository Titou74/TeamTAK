//! Connected devices: what TAK Server lists as open sessions, matched with the platform's
//! accesses and teams.
//!
//! The list is read by the monitoring loop and kept in memory; a request only joins it
//! with the database. TAK Server does not say since when a device is connected: the
//! platform notes when it first saw each session, so "since" is exact to one look of the
//! loop, and cannot be older than the platform's own start.

use std::collections::BTreeMap;
use std::sync::RwLock;

use axum::extract::{Path, State};
use axum::Json;
use serde_json::{json, Value};
use tak_manager_core::policy::{can, Action};
use tak_manager_core::tak::Subscription;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::auth::{AdminAccount, CurrentAccount};
use crate::error::{ApiError, ApiResult};
use crate::state::SharedState;
use crate::teams::team_context;

/// A device that has sent nothing for this long is shown as silent. TAK Server keeps
/// listing sessions for a while after their connection is gone.
const SILENT_AFTER: time::Duration = time::Duration::minutes(10);

#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    pub subscription: Subscription,
    /// When the platform first saw this session.
    pub first_seen: OffsetDateTime,
}

#[derive(Debug, Default)]
struct Board {
    /// When TAK Server last answered.
    as_of: Option<OffsetDateTime>,
    /// When the platform started watching: a session seen at that very look may be older.
    watching_since: Option<OffsetDateTime>,
    sessions: BTreeMap<String, Session>,
}

#[derive(Debug, Default)]
pub struct DeviceBoard {
    inner: RwLock<Board>,
}

impl DeviceBoard {
    /// Replaces the list with what TAK Server lists now, keeping the first-seen time of the
    /// sessions that were already there.
    pub fn update(&self, subscriptions: Vec<Subscription>, now: OffsetDateTime) {
        let mut board = self.inner.write().unwrap();
        let mut sessions = BTreeMap::new();
        for subscription in subscriptions {
            let first_seen = board.sessions.get(&subscription.subscription_uid).map(|s| s.first_seen).unwrap_or(now);
            sessions.insert(subscription.subscription_uid.clone(), Session { subscription, first_seen });
        }
        board.sessions = sessions;
        board.watching_since.get_or_insert(now);
        board.as_of = Some(now);
    }

    fn read(&self) -> (Option<OffsetDateTime>, Option<OffsetDateTime>, Vec<Session>) {
        let board = self.inner.read().unwrap();
        (board.as_of, board.watching_since, board.sessions.values().cloned().collect())
    }
}

#[derive(sqlx::FromRow)]
struct AccessRow {
    username: String,
    kind: String,
    label: String,
    team_id: Uuid,
    team_name: String,
    owner_email: Option<String>,
    owner_name: Option<String>,
}

fn rfc3339(at: OffsetDateTime) -> String {
    at.format(&time::format_description::well_known::Rfc3339).unwrap_or_default()
}

/// One device as the API shows it. The address it connects from is for administrators only.
fn device_view(session: &Session, access: Option<&AccessRow>, watching_since: Option<OffsetDateTime>, now: OffsetDateTime, with_address: bool) -> Value {
    let s = &session.subscription;
    let mut view = json!({
        "id": s.subscription_uid,
        "username": s.username,
        "callsign": s.callsign,
        "client": s.client,
        "groups": s.groups,
        "connected_since": rfc3339(session.first_seen),
        // Seen at the platform's first look: it was connected before, since when is not known.
        "since_is_lower_bound": watching_since == Some(session.first_seen),
        "last_report_at": s.last_report_at.map(rfc3339),
        "silent": s.last_report_at.is_some_and(|at| now - at > SILENT_AFTER),
        "access": access.map(|a| json!({
            "kind": a.kind,
            "label": a.label,
            "owner": a.owner_name.as_deref().filter(|n| !n.is_empty()).or(a.owner_email.as_deref()),
            "team_id": a.team_id,
            "team_name": a.team_name,
        })),
    });
    if with_address {
        view["ip_address"] = json!(s.ip_address);
    }
    view
}

async fn accesses(state: &SharedState, team_id: Option<Uuid>) -> ApiResult<Vec<AccessRow>> {
    Ok(sqlx::query_as(
        "SELECT a.username, a.kind, a.label, t.id AS team_id, t.name AS team_name, o.email AS owner_email, o.display_name AS owner_name
           FROM tak_accesses a JOIN teams t ON t.id = a.team_id LEFT JOIN accounts o ON o.id = a.account_id
          WHERE $1::uuid IS NULL OR a.team_id = $1",
    )
    .bind(team_id)
    .fetch_all(&state.db)
    .await?)
}

fn sorted(mut devices: Vec<Value>) -> Vec<Value> {
    devices.sort_by(|a, b| {
        let key = |v: &Value| (v["access"]["team_name"].as_str().unwrap_or("").to_lowercase(), v["username"].as_str().unwrap_or("").to_string(), v["connected_since"].as_str().unwrap_or("").to_string());
        key(a).cmp(&key(b))
    });
    devices
}

/// `GET /api/admin/devices`: every device connected to TAK Server.
pub async fn admin_devices(State(state): State<SharedState>, AdminAccount(admin): AdminAccount) -> ApiResult<Json<Value>> {
    admin.require_platform(Action::ViewAllDevices)?;
    let (as_of, watching_since, sessions) = state.monitor.devices.read();
    let known = accesses(&state, None).await?;
    let now = OffsetDateTime::now_utc();
    let (identified, unidentified): (Vec<&Session>, Vec<&Session>) = sessions.iter().partition(|s| !s.subscription.username.is_empty());
    let devices = identified
        .into_iter()
        .map(|s| device_view(s, known.iter().find(|a| a.username == s.subscription.username), watching_since, now, true))
        .collect();
    Ok(Json(json!({
        "as_of": as_of.map(rfc3339),
        "devices": sorted(devices),
        // Sessions without any group: TAK Server lists them without their account, and
        // keeps listing them after they are gone.
        "unidentified": unidentified.into_iter().map(|s| device_view(s, None, watching_since, now, true)).collect::<Vec<_>>(),
    })))
}

/// `GET /api/teams/{id}/devices`: the devices connected with the team's accesses, for its
/// leaders (and for administrators). With a shared identity this is the only way to know
/// how many devices use it.
pub async fn team_devices(State(state): State<SharedState>, CurrentAccount(account): CurrentAccount, Path(team_id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let ctx = team_context(&state, team_id, &account).await?;
    if !can(ctx.actor, Action::ViewTeamDevices) {
        return Err(ApiError::Forbidden);
    }
    let (as_of, watching_since, sessions) = state.monitor.devices.read();
    let known = accesses(&state, Some(team_id)).await?;
    let now = OffsetDateTime::now_utc();
    let devices = sessions
        .iter()
        .filter_map(|s| known.iter().find(|a| a.username == s.subscription.username).map(|a| device_view(s, Some(a), watching_since, now, false)))
        .collect();
    Ok(Json(json!({ "as_of": as_of.map(rfc3339), "devices": sorted(devices) })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn sub(uid: &str, username: &str) -> Subscription {
        Subscription { subscription_uid: uid.into(), username: username.into(), ..Default::default() }
    }

    #[test]
    fn a_session_keeps_the_time_it_was_first_seen() {
        let board = DeviceBoard::default();
        let t0 = datetime!(2026-11-19 12:00 UTC);
        let t1 = t0 + time::Duration::minutes(1);
        let t2 = t0 + time::Duration::minutes(2);
        board.update(vec![sub("tls:1", "alpha-shared")], t0);
        board.update(vec![sub("tls:1", "alpha-shared"), sub("tls:2", "alpha-shared")], t1);
        let (as_of, watching_since, sessions) = board.read();
        assert_eq!((as_of, watching_since), (Some(t1), Some(t0)));
        let seen: Vec<(&str, OffsetDateTime)> = sessions.iter().map(|s| (s.subscription.subscription_uid.as_str(), s.first_seen)).collect();
        assert_eq!(seen, vec![("tls:1", t0), ("tls:2", t1)], "two devices on one shared identity are two sessions");
        // The first one disconnects and comes back: a new session, a new time.
        board.update(vec![sub("tls:2", "alpha-shared")], t2);
        board.update(vec![sub("tls:2", "alpha-shared"), sub("tls:3", "alpha-shared")], t2 + time::Duration::minutes(1));
        let (_, _, sessions) = board.read();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions.iter().find(|s| s.subscription.subscription_uid == "tls:3").unwrap().first_seen, t2 + time::Duration::minutes(1));
    }

    #[test]
    fn the_address_is_only_in_the_administrator_view() {
        let now = datetime!(2026-11-19 12:30 UTC);
        let session = Session {
            subscription: Subscription {
                subscription_uid: "tls:9".into(),
                username: "alpha-shared".into(),
                callsign: Some("ALPHA-1".into()),
                ip_address: Some("203.0.113.9".into()),
                last_report_at: Some(now - time::Duration::minutes(11)),
                groups: vec!["alpha".into()],
                ..Default::default()
            },
            first_seen: datetime!(2026-11-19 12:00 UTC),
        };
        let access = AccessRow { username: "alpha-shared".into(), kind: "shared".into(), label: String::new(), team_id: Uuid::nil(), team_name: "Alpha".into(), owner_email: None, owner_name: None };
        let for_admin = device_view(&session, Some(&access), Some(session.first_seen), now, true);
        assert_eq!(for_admin["ip_address"], "203.0.113.9");
        assert_eq!((for_admin["silent"].clone(), for_admin["since_is_lower_bound"].clone()), (json!(true), json!(true)));
        let for_leader = device_view(&session, Some(&access), None, now, false);
        assert!(for_leader.get("ip_address").is_none());
        assert_eq!((for_leader["callsign"].clone(), for_leader["access"]["team_name"].clone()), (json!("ALPHA-1"), json!("Alpha")));
        assert_eq!(for_leader["since_is_lower_bound"], false);
    }
}
