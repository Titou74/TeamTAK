//! Errors returned by the API: a stable code and an HTTP status, never a sentence. The UI
//! translates the code (`errors.<code>` in its resource files).

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

macro_rules! api_errors {
    ($( $(#[$doc:meta])* $variant:ident => ($code:literal, $status:ident) ),+ $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum ApiError {
            $( $(#[$doc])* $variant ),+
        }

        impl ApiError {
            /// Every error the API can return, for the check that each one is translated.
            #[cfg_attr(not(test), allow(dead_code))]
            pub const ALL: &'static [ApiError] = &[ $( ApiError::$variant ),+ ];

            pub fn code(self) -> &'static str {
                match self { $( ApiError::$variant => $code ),+ }
            }

            pub fn status(self) -> StatusCode {
                match self { $( ApiError::$variant => StatusCode::$status ),+ }
            }
        }
    };
}

api_errors! {
    /// No valid session.
    Unauthenticated => ("unauthenticated", UNAUTHORIZED),
    /// Signed in, but not allowed to do this.
    Forbidden => ("forbidden", FORBIDDEN),
    NotFound => ("not_found", NOT_FOUND),
    /// The request body or a parameter is not what the API expects.
    InvalidInput => ("invalid_input", BAD_REQUEST),
    InvalidEmail => ("invalid_email", BAD_REQUEST),
    /// A team name that yields no usable TAK group name, or a reserved one.
    InvalidTeamName => ("invalid_team_name", BAD_REQUEST),
    /// Too many login codes or attempts; try again later.
    RateLimited => ("rate_limited", TOO_MANY_REQUESTS),
    /// Wrong, expired or already used login code.
    InvalidCode => ("invalid_code", BAD_REQUEST),
    /// The e-mail could not be handed to the mail server.
    MailUnavailable => ("mail_unavailable", SERVICE_UNAVAILABLE),
    TeamNameTaken => ("team_name_taken", CONFLICT),
    /// The account already has the maximum number of requests waiting for approval.
    TooManyPendingRequests => ("too_many_pending_requests", CONFLICT),
    /// The team is pending, rejected or suspended.
    TeamNotActive => ("team_not_active", CONFLICT),
    /// The request was already approved or rejected.
    TeamAlreadyDecided => ("team_already_decided", CONFLICT),
    /// Members and pending invitations have reached the team's cap.
    TeamFull => ("team_full", CONFLICT),
    AlreadyMember => ("already_member", CONFLICT),
    AlreadyInvited => ("already_invited", CONFLICT),
    InvitationNotPending => ("invitation_not_pending", CONFLICT),
    /// A team must keep at least one leader.
    LastLeader => ("last_leader", CONFLICT),
    AccessExists => ("access_exists", CONFLICT),
    /// This kind of access does not fit the team's connection mode.
    AccessKindNotAllowed => ("access_kind_not_allowed", CONFLICT),
    /// TAK Server could not be reached or refused; nothing was changed, try again.
    TakUnavailable => ("tak_unavailable", SERVICE_UNAVAILABLE),
    DirectoryUnavailable => ("directory_unavailable", SERVICE_UNAVAILABLE),
    /// An event name that yields no usable TAK group name.
    InvalidEventName => ("invalid_event_name", BAD_REQUEST),
    /// End before the start, already past, too short, too long, or a start moved after the
    /// event began.
    InvalidEventDates => ("invalid_event_dates", BAD_REQUEST),
    /// Another event that is not over has the same TAK group name.
    EventNameTaken => ("event_name_taken", CONFLICT),
    /// The team already hosts the maximum number of events that are not over.
    TooManyEvents => ("too_many_events", CONFLICT),
    /// The event has ended or was cancelled: nothing about it can change.
    EventClosed => ("event_closed", CONFLICT),
    /// Only a running event can be ended early.
    EventNotRunning => ("event_not_running", CONFLICT),
    /// The team is already invited to the event, or already takes part in it.
    TeamAlreadyInEvent => ("team_already_in_event", CONFLICT),
    /// No invitation to this event is waiting for the team's answer.
    EventInvitationNotPending => ("event_invitation_not_pending", CONFLICT),
    /// The host agent is not installed or does not answer: nothing was done.
    AgentUnavailable => ("agent_unavailable", SERVICE_UNAVAILABLE),
    /// The host agent is in the middle of another action (a backup, a restart).
    AgentBusy => ("agent_busy", CONFLICT),
    /// Not a service of this stack that can be restarted from the web UI.
    ServiceNotAllowed => ("service_not_allowed", BAD_REQUEST),
    /// The host agent, or the webhook, tried and failed; the server's log has the reason.
    ActionFailed => ("action_failed", BAD_GATEWAY),
    /// No webhook address is set.
    WebhookNotSet => ("webhook_not_set", CONFLICT),
    Internal => ("internal", INTERNAL_SERVER_ERROR),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status(), Json(json!({ "error": self.code() }))).into_response()
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        match e {
            sqlx::Error::RowNotFound => ApiError::NotFound,
            other => {
                tracing::error!(error = %other, "database error");
                ApiError::Internal
            }
        }
    }
}

impl From<tak_manager_core::secret::SecretError> for ApiError {
    fn from(e: tak_manager_core::secret::SecretError) -> Self {
        tracing::error!(error = %e, "stored secret unreadable");
        ApiError::Internal
    }
}

impl From<tak_manager_core::package::PackageError> for ApiError {
    fn from(e: tak_manager_core::package::PackageError) -> Self {
        tracing::error!(error = %e, "connection package");
        ApiError::Internal
    }
}

impl From<tak_manager_core::tak::TakError> for ApiError {
    fn from(e: tak_manager_core::tak::TakError) -> Self {
        tracing::warn!(error = %e, "TAK Server call failed");
        ApiError::TakUnavailable
    }
}

impl From<tak_manager_core::events::EventError> for ApiError {
    fn from(e: tak_manager_core::events::EventError) -> Self {
        use tak_manager_core::events::EventError;
        match e {
            EventError::Closed => ApiError::EventClosed,
            EventError::InvalidDates => ApiError::InvalidEventDates,
            EventError::NotRunning => ApiError::EventNotRunning,
            EventError::AlreadyInEvent => ApiError::TeamAlreadyInEvent,
            EventError::NotInvited => ApiError::EventInvitationNotPending,
            // A team that does not take part is not in the event for the caller.
            EventError::NotParticipating => ApiError::NotFound,
        }
    }
}

pub type ApiResult<T> = Result<T, ApiError>;

/// True when a database error is a violation of the named unique index or constraint.
pub fn is_unique_violation(e: &sqlx::Error, constraint: &str) -> bool {
    match e {
        sqlx::Error::Database(db) => db.is_unique_violation() && db.constraint() == Some(constraint),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn codes_are_unique_snake_case() {
        let codes: BTreeSet<&str> = ApiError::ALL.iter().map(|e| e.code()).collect();
        assert_eq!(codes.len(), ApiError::ALL.len());
        for code in codes {
            assert!(code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'), "{code}");
        }
    }

    /// Section 6 of the plan: the API returns codes, the UI translates them. Every code must
    /// have a sentence in every language of the UI.
    #[test]
    fn every_error_code_is_translated_in_every_language() {
        let locales = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/src/locales");
        let mut languages = 0;
        for entry in std::fs::read_dir(&locales).unwrap() {
            let file = entry.unwrap().path().join("translation.json");
            if !file.is_file() {
                continue;
            }
            languages += 1;
            let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
            for error in ApiError::ALL {
                let text = json.get("errors").and_then(|e| e.get(error.code())).and_then(|v| v.as_str());
                assert!(text.is_some_and(|t| !t.trim().is_empty()), "errors.{} missing in {}", error.code(), file.display());
            }
        }
        assert!(languages >= 2, "expected at least English and French in {}", locales.display());
    }

    /// Same rule for what the reconciliation reports to the administrator.
    #[test]
    fn every_reconciliation_change_is_translated_in_every_language() {
        let locales = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/src/locales");
        for entry in std::fs::read_dir(&locales).unwrap() {
            let file = entry.unwrap().path().join("translation.json");
            if !file.is_file() {
                continue;
            }
            let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
            for kind in tak_manager_core::reconcile::CHANGE_KINDS {
                let text = json.pointer(&format!("/reconcile/kinds/{kind}")).and_then(|v| v.as_str());
                assert!(text.is_some_and(|t| !t.trim().is_empty()), "reconcile.kinds.{kind} missing in {}", file.display());
            }
            // And for the origin of each pass, shown next to it.
            for trigger in crate::reconciler::Trigger::ALL {
                let name = trigger.as_str();
                let text = json.pointer(&format!("/reconcile/triggers/{name}")).and_then(|v| v.as_str());
                assert!(text.is_some_and(|t| !t.trim().is_empty()), "reconcile.triggers.{name} missing in {}", file.display());
            }
        }
    }
}
