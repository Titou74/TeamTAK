//! Languages of the platform and the texts of its e-mails.
//!
//! The texts are compiled in from `web/src/locales/<language>/email.json` (see build.rs).
//! The list of languages is the set of folders that hold such a file: adding a language to
//! the UI adds it here too.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde_json::Value;
use time::OffsetDateTime;

include!(concat!(env!("OUT_DIR"), "/email_locales.rs"));

/// Default and fallback language.
pub const DEFAULT_LANGUAGE: &str = "en";

fn catalogs() -> &'static BTreeMap<&'static str, Value> {
    static CATALOGS: OnceLock<BTreeMap<&'static str, Value>> = OnceLock::new();
    CATALOGS.get_or_init(|| {
        EMAIL_LOCALES
            .iter()
            .map(|(language, json)| {
                (*language, serde_json::from_str(json).unwrap_or_else(|e| panic!("email.json of {language}: {e}")))
            })
            .collect()
    })
}

/// Languages the platform supports, in alphabetical order.
pub fn languages() -> Vec<&'static str> {
    catalogs().keys().copied().collect()
}

/// The language itself when supported, the default one otherwise.
pub fn resolve(language: &str) -> &'static str {
    catalogs().keys().copied().find(|l| *l == language).unwrap_or(DEFAULT_LANGUAGE)
}

pub fn is_supported(language: &str) -> bool {
    catalogs().contains_key(language)
}

fn lookup(language: &str, path: &[&str]) -> Option<&'static str> {
    let mut node = catalogs().get(language)?;
    for key in path {
        node = node.get(key)?;
    }
    node.as_str()
}

/// Text at `path` in `language`, falling back to English.
fn text(language: &str, path: &[&str]) -> &'static str {
    lookup(language, path)
        .or_else(|| lookup(DEFAULT_LANGUAGE, path))
        .unwrap_or_else(|| panic!("e-mail text {} missing in the default language", path.join(".")))
}

/// A date and time written the way `language` writes them, in UTC and saying so.
///
/// The server does not know where the reader is, so an e-mail gives UTC; the site shows the
/// same dates in the reader's own time zone. The format comes from the language's
/// `email.json` (`datetime_format`, in the notation of the `time` crate).
pub fn format_datetime(language: &str, at: OffsetDateTime) -> String {
    let pattern = text(resolve(language), &["datetime_format"]);
    let utc = at.to_offset(time::UtcOffset::UTC);
    time::format_description::parse_borrowed::<2>(pattern).ok().and_then(|format| utc.format(&format).ok()).unwrap_or_else(|| utc.to_string())
}

fn fill(template: &str, vars: &[(&str, &str)]) -> String {
    let mut out = template.to_string();
    for (name, value) in vars {
        out = out.replace(&format!("{{{{{name}}}}}"), value);
    }
    out
}

/// The e-mails the platform sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Template {
    LoginCode,
    Invitation,
    TeamRequest,
    TeamApproved,
    TeamRejected,
    /// To the leaders of a team invited to an event.
    EventInvitation,
    /// To the leaders of the host: an invited team accepted, declined, or left.
    EventAccepted,
    EventDeclined,
    EventLeft,
    /// To the leaders of every team concerned: the event was called off, its dates
    /// changed, or it was ended before its time.
    EventCancelled,
    EventRescheduled,
    EventEndedEarly,
    /// To the leaders of a team the host took out of an event.
    EventTeamRemoved,
    /// To every administrator: alerts raised, resolved, or still open. Its subject and its
    /// list are built by [`render_alerts`].
    AlertDigest,
    /// To the leaders of a team: a certificate shipped in one of its zips nears its end.
    CertificateNotice,
}

impl Template {
    #[cfg_attr(not(test), allow(dead_code))]
    pub const ALL: &'static [Template] = &[
        Template::LoginCode,
        Template::Invitation,
        Template::TeamRequest,
        Template::TeamApproved,
        Template::TeamRejected,
        Template::EventInvitation,
        Template::EventAccepted,
        Template::EventDeclined,
        Template::EventLeft,
        Template::EventCancelled,
        Template::EventRescheduled,
        Template::EventEndedEarly,
        Template::EventTeamRemoved,
        Template::AlertDigest,
        Template::CertificateNotice,
    ];

    pub fn key(self) -> &'static str {
        match self {
            Template::LoginCode => "login_code",
            Template::Invitation => "invitation",
            Template::TeamRequest => "team_request",
            Template::TeamApproved => "team_approved",
            Template::TeamRejected => "team_rejected",
            Template::EventInvitation => "event_invitation",
            Template::EventAccepted => "event_accepted",
            Template::EventDeclined => "event_declined",
            Template::EventLeft => "event_left",
            Template::EventCancelled => "event_cancelled",
            Template::EventRescheduled => "event_rescheduled",
            Template::EventEndedEarly => "event_ended_early",
            Template::EventTeamRemoved => "event_team_removed",
            Template::AlertDigest => "alert_digest",
            Template::CertificateNotice => "certificate_notice",
        }
    }
}

pub struct Rendered {
    pub subject: String,
    pub body: String,
}

/// Renders an e-mail in `language` (English when the language is unknown). `vars` replace
/// the `{{name}}` placeholders; `url` is also used by the signature.
pub fn render(template: Template, language: &str, vars: &[(&str, &str)], url: &str) -> Rendered {
    let language = resolve(language);
    // A subject is a single line whatever the variables hold.
    let subject = fill(text(language, &[template.key(), "subject"]), vars).replace(['\r', '\n'], " ");
    let body = fill(text(language, &[template.key(), "body"]), vars);
    let signature = fill(text(language, &["signature"]), &[("url", url)]);
    Rendered { subject, body: format!("{body}\n\n-- \n{signature}\n") }
}

// ------------------------------------------------------------------------------- alerts

/// One alert as a notification mentions it.
#[derive(Debug, Clone, PartialEq)]
pub struct AlertLine {
    /// Stable code of the kind (`service_down`).
    pub kind: String,
    /// What it is about: a service, a filesystem, a certificate; empty for most kinds.
    pub subject: String,
    /// `low`, `warning` or `critical`.
    pub severity: String,
    /// Figures the sentence is filled with.
    pub details: Value,
}

/// What one evaluation has to say: everything in one message.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AlertBatch {
    pub raised: Vec<AlertLine>,
    pub resolved: Vec<AlertLine>,
    /// Still open, last mentioned long enough ago.
    pub reminded: Vec<AlertLine>,
}

impl AlertBatch {
    pub fn is_empty(&self) -> bool {
        self.raised.is_empty() && self.resolved.is_empty() && self.reminded.is_empty()
    }
}

fn alert_text(language: &str, path: &[&str]) -> Option<&'static str> {
    lookup(language, path).or_else(|| lookup(DEFAULT_LANGUAGE, path))
}

/// The sentence of an alert in `language`. A kind or a subject without a text is written
/// as its code: a notification is never dropped for a missing translation.
pub fn alert_sentence(language: &str, line: &AlertLine) -> String {
    let language = resolve(language);
    let Some(template) = alert_text(language, &["alerts", "kinds", &line.kind]) else {
        return format!("{} {}", line.kind, line.subject).trim().to_string();
    };
    // Subjects that are codes of the platform (a certificate, a filesystem) have a name;
    // the others (a service, an event's group) are shown as they are.
    let subject = alert_text(language, &["alerts", "subjects", &line.subject]).unwrap_or(&line.subject).to_string();
    let mut vars: Vec<(String, String)> = vec![("subject".to_string(), subject)];
    if let Value::Object(details) = &line.details {
        for (key, value) in details {
            let shown = match value {
                Value::String(text) => text.clone(),
                // Numbers are written the way the language writes them: "98,6" in French.
                Value::Number(number) => number.to_string().replace('.', text(language, &["decimal_separator"])),
                other => other.to_string(),
            };
            vars.push((key.clone(), shown));
        }
    }
    let borrowed: Vec<(&str, &str)> = vars.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    fill(template, &borrowed)
}

/// The e-mail that tells an administrator what one evaluation found, in their language.
/// A single alert is named in the subject; several are counted.
pub fn render_alerts(language: &str, batch: &AlertBatch, url: &str) -> Rendered {
    let lang = resolve(language);
    let word = |key: &str| text(lang, &["alerts", key]);
    let line = |alert: &AlertLine| {
        let severity = alert_text(lang, &["alerts", "severity", &alert.severity]).unwrap_or(&alert.severity);
        format!("  - [{severity}] {}", alert_sentence(lang, alert))
    };
    let mut sections = Vec::new();
    for (heading, alerts) in [("heading_raised", &batch.raised), ("heading_resolved", &batch.resolved), ("heading_reminded", &batch.reminded)] {
        if !alerts.is_empty() {
            sections.push(format!("{}\n{}", word(heading), alerts.iter().map(line).collect::<Vec<_>>().join("\n")));
        }
    }
    let subject = match (&batch.raised[..], &batch.resolved[..], &batch.reminded[..]) {
        ([only], [], []) => fill(word("subject_raised"), &[("text", &alert_sentence(lang, only))]),
        ([], [only], []) => fill(word("subject_resolved"), &[("text", &alert_sentence(lang, only))]),
        _ => fill(
            word("subject_several"),
            &[
                ("raised", &batch.raised.len().to_string()),
                ("resolved", &batch.resolved.len().to_string()),
                ("reminded", &batch.reminded.len().to_string()),
            ],
        ),
    };
    render(Template::AlertDigest, lang, &[("subject", &subject), ("sections", &sections.join("\n\n")), ("url", url)], url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn placeholders(s: &str) -> BTreeSet<String> {
        s.split("{{").skip(1).filter_map(|p| p.split_once("}}")).map(|(name, _)| name.to_string()).collect()
    }

    fn leaves(prefix: &str, v: &Value, out: &mut BTreeMap<String, String>) {
        match v {
            Value::Object(map) => {
                for (k, v) in map {
                    let key = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
                    leaves(&key, v, out);
                }
            }
            Value::String(s) => {
                out.insert(prefix.to_string(), s.clone());
            }
            other => panic!("{prefix}: unexpected value {other}"),
        }
    }

    #[test]
    fn english_and_french_are_present() {
        assert!(is_supported("en") && is_supported("fr"));
        assert_eq!(resolve("fr"), "fr");
        assert_eq!(resolve("xx"), "en");
    }

    #[test]
    fn every_language_has_the_same_keys_and_placeholders() {
        let mut reference = BTreeMap::new();
        leaves("", &catalogs()["en"], &mut reference);
        for (language, catalog) in catalogs() {
            let mut texts = BTreeMap::new();
            leaves("", catalog, &mut texts);
            assert_eq!(
                texts.keys().collect::<Vec<_>>(),
                reference.keys().collect::<Vec<_>>(),
                "keys of {language}/email.json differ from en"
            );
            for (key, value) in &texts {
                assert_eq!(placeholders(value), placeholders(&reference[key]), "placeholders of {key} in {language}");
            }
        }
    }

    #[test]
    fn every_template_has_its_texts() {
        for template in Template::ALL {
            for language in languages() {
                assert!(lookup(language, &[template.key(), "subject"]).is_some(), "{language} {}", template.key());
                assert!(lookup(language, &[template.key(), "body"]).is_some(), "{language} {}", template.key());
            }
        }
    }

    #[test]
    fn login_code_is_rendered_in_the_recipient_language() {
        let vars = [("code", "123456"), ("minutes", "10")];
        let en = render(Template::LoginCode, "en", &vars, "https://tak.example.org");
        let fr = render(Template::LoginCode, "fr", &vars, "https://tak.example.org");
        assert_eq!(en.subject, "Your sign-in code: 123456");
        assert_eq!(fr.subject, "Votre code de connexion : 123456");
        assert!(fr.body.contains("valable 10 minutes"));
        assert!(en.body.contains("https://tak.example.org"));
        assert!(!en.body.contains("{{") && !fr.body.contains("{{"));
        let fallback = render(Template::LoginCode, "de", &vars, "u");
        assert_eq!(fallback.subject, en.subject);
    }

    #[test]
    fn dates_are_written_in_the_recipient_language_and_in_utc() {
        let at = time::macros::datetime!(2026-11-20 15:05 +01:00);
        assert_eq!(format_datetime("en", at), "2026-11-20 14:05 UTC");
        assert_eq!(format_datetime("fr", at), "20/11/2026 à 14:05 UTC");
        assert_eq!(format_datetime("xx", at), format_datetime("en", at), "unknown language: English");
        for language in languages() {
            assert!(format_datetime(language, at).contains("UTC"), "{language}: the time zone must be said");
        }
    }

    #[test]
    fn event_emails_are_complete_in_every_language() {
        let vars = [
            ("event", "Exercice Mont-Blanc"),
            ("channel", "evt-exercice-mont-blanc"),
            ("host", "Alpha"),
            ("team", "Bravo"),
            ("start", "20/11/2026 à 14:05 UTC"),
            ("end", "21/11/2026 à 18:00 UTC"),
            ("url", "https://tak.example.org/teams/1"),
        ];
        let event_templates: Vec<Template> = Template::ALL.iter().copied().filter(|t| t.key().starts_with("event_")).collect();
        assert_eq!(event_templates.len(), 8);
        for template in event_templates {
            for language in languages() {
                let mail = render(template, language, &vars, "https://tak.example.org");
                let all = format!("{}\n{}", mail.subject, mail.body);
                assert!(!all.contains("{{"), "{language} {}: a placeholder was left: {all}", template.key());
                assert!(mail.subject.contains("Exercice Mont-Blanc"), "{language} {}", template.key());
                assert!(mail.body.contains("https://tak.example.org/teams/1"), "{language} {}", template.key());
            }
        }
        let fr = render(Template::EventInvitation, "fr", &vars, "u");
        assert!(fr.subject.contains("Alpha") && fr.body.contains("20/11/2026 à 14:05 UTC") && fr.body.contains("evt-exercice-mont-blanc"));
        let en = render(Template::EventAccepted, "en", &vars, "u");
        assert!(en.subject.contains("Bravo"), "{}", en.subject);
    }

    /// One finding of every kind, with the details the rules really produce.
    fn every_kind_of_finding() -> Vec<AlertLine> {
        use tak_manager_core::alerts::*;
        use time::Duration;
        let now = time::macros::datetime!(2026-11-19 12:00 UTC);
        let service = |name: &str, state: Option<&str>, health: Option<&str>, restarts| ServiceObservation {
            name: name.into(),
            state: state.map(str::to_string),
            health: health.map(str::to_string),
            recent_restarts: restarts,
        };
        let obs = Observations {
            agent: AgentState::Unreachable,
            disks: Some(vec![("root".into(), 93.4)]),
            memory_used_percent: Some(95.0),
            services: Some(vec![
                service("caddy", Some("exited"), None, 0),
                service("openldap", Some("running"), Some("unhealthy"), 0),
                service("takserver", Some("restarting"), None, 4),
            ]),
            tak_processes: Some(vec![("messaging".into(), false)]),
            security_updates: Some(9),
            reboot_required: Some(true),
            clock_synchronized: Some(false),
            tak_failures: Some(vec!["8089".into(), "api".into()]),
            directory_reachable: Some(false),
            reconcile: Some(ReconcileObservation { last_ok_at: Some(now - Duration::minutes(42)), running_since: None }),
            overdue_events: Some(vec![
                OverdueEvent { group: "evt-drill".into(), name: "Drill".into(), boundary: Boundary::Start, due_at: now - Duration::minutes(7) },
                OverdueEvent { group: "evt-night".into(), name: "Night".into(), boundary: Boundary::End, due_at: now - Duration::minutes(9) },
            ]),
            certificates: Some(vec![
                CertificateObservation { name: "tak_server".into(), expires_at: now + Duration::days(12) },
                CertificateObservation { name: "platform_api".into(), expires_at: now - Duration::days(3) },
            ]),
            client_certificates: Some((5, 2)),
            backups: Some(BackupObservation { enabled: true, newest_at: Some(now - Duration::hours(50)), since: None, last_run_failed: true }),
            mail: Some(MailObservation { failures_last_hour: 6, last_failure_at: Some(now), last_success_at: None }),
            ..Observations::empty(now)
        };
        let findings = evaluate(&obs, &Thresholds::default()).findings;
        let kinds: BTreeSet<&str> = findings.iter().map(|f| f.kind.code()).collect();
        assert_eq!(kinds.len(), AlertKind::ALL.len(), "this list must exercise every kind; it has {kinds:?}");
        findings
            .iter()
            .map(|f| AlertLine { kind: f.kind.code().into(), subject: f.subject.clone(), severity: f.severity.as_str().into(), details: serde_json::json!(f.details) })
            .collect()
    }

    /// Alert kinds are stable codes; each has a sentence in every language, for the e-mails
    /// (email.json) and for the dashboard (translation.json), and so has each severity.
    #[test]
    fn every_alert_kind_and_severity_is_translated_in_every_language() {
        use tak_manager_core::alerts::{AlertKind, Severity};
        let locales = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/src/locales");
        for language in languages() {
            let ui: Value = serde_json::from_str(&std::fs::read_to_string(locales.join(language).join("translation.json")).unwrap()).unwrap();
            for kind in AlertKind::ALL {
                assert!(lookup(language, &["alerts", "kinds", kind.code()]).is_some_and(|t| !t.trim().is_empty()), "{language}/email.json: alerts.kinds.{}", kind.code());
                let text = ui.pointer(&format!("/alerts/kinds/{}", kind.code())).and_then(|v| v.as_str());
                assert!(text.is_some_and(|t| !t.trim().is_empty()), "{language}/translation.json: alerts.kinds.{}", kind.code());
            }
            for severity in Severity::ALL {
                assert!(lookup(language, &["alerts", "severity", severity.as_str()]).is_some(), "{language}/email.json: alerts.severity.{}", severity.as_str());
                assert!(ui.pointer(&format!("/alerts/severity/{}", severity.as_str())).is_some(), "{language}/translation.json: alerts.severity.{}", severity.as_str());
            }
        }
    }

    #[test]
    fn every_alert_sentence_is_complete_in_every_language() {
        for line in every_kind_of_finding() {
            for language in languages() {
                let sentence = alert_sentence(language, &line);
                assert!(!sentence.contains("{{"), "{language} {}: a placeholder was left: {sentence}", line.kind);
                assert!(sentence.chars().count() > 15, "{language} {}: {sentence}", line.kind);
            }
        }
    }

    /// The same sentences are shown by the dashboard: they must use the same figures.
    #[test]
    fn dashboard_and_email_sentences_use_the_same_placeholders() {
        let locales = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/src/locales");
        for language in languages() {
            let ui: Value = serde_json::from_str(&std::fs::read_to_string(locales.join(language).join("translation.json")).unwrap()).unwrap();
            for kind in tak_manager_core::alerts::AlertKind::ALL {
                let mail = lookup(language, &["alerts", "kinds", kind.code()]).unwrap();
                let page = ui.pointer(&format!("/alerts/kinds/{}", kind.code())).and_then(|v| v.as_str()).unwrap_or_default();
                assert_eq!(placeholders(page), placeholders(mail), "{language}: alerts.kinds.{}", kind.code());
            }
        }
    }

    #[test]
    fn alert_sentences_carry_their_figures_and_translated_subjects() {
        let line = |kind: &str, subject: &str, details: Value| AlertLine { kind: kind.into(), subject: subject.into(), severity: "warning".into(), details };
        let disk = line("disk_full", "docker", serde_json::json!({ "percent": 93.4 }));
        assert_eq!(alert_sentence("en", &disk), "Disk nearly full: Docker's disk is 93.4 % used.");
        assert_eq!(alert_sentence("fr", &disk), "Disque presque plein : le disque de Docker est utilisé à 93,4 %.", "a decimal comma in French");
        let cert = line("certificate_expiring", "tak_server", serde_json::json!({ "days": 12 }));
        assert_eq!(alert_sentence("en", &cert), "Certificate expiring in 12 days: TAK Server.");
        let service = line("service_down", "caddy", serde_json::json!({ "state": "exited" }));
        assert_eq!(alert_sentence("fr", &service), "Le service caddy ne tourne pas (état : exited).");
        // A kind added later without its text is still said, by its code.
        assert_eq!(alert_sentence("en", &line("brand_new_kind", "x", Value::Null)), "brand_new_kind x");
        assert_eq!(alert_sentence("xx", &service), alert_sentence("en", &service), "unknown language: English");
    }

    #[test]
    fn one_alert_is_named_in_the_subject_and_several_are_counted() {
        let line = |kind: &str, subject: &str, severity: &str, details: Value| AlertLine { kind: kind.into(), subject: subject.into(), severity: severity.into(), details };
        let down = line("service_down", "caddy", "critical", serde_json::json!({ "state": "exited" }));
        let raised = AlertBatch { raised: vec![down.clone()], ..Default::default() };
        let en = render_alerts("en", &raised, "https://tak.example.org/monitoring");
        assert_eq!(en.subject, "[TAK Manager] Alert: The service caddy is not running (state: exited).");
        assert!(en.body.contains("New alerts:\n  - [critical] The service caddy is not running (state: exited)."), "{}", en.body);
        assert!(en.body.contains("https://tak.example.org/monitoring"));
        let fr = render_alerts("fr", &raised, "u");
        assert_eq!(fr.subject, "[TAK Manager] Alerte : Le service caddy ne tourne pas (état : exited).");
        assert!(fr.body.contains("Nouvelles alertes :\n  - [critique] Le service caddy ne tourne pas"), "{}", fr.body);

        let resolved = AlertBatch { resolved: vec![down.clone()], ..Default::default() };
        assert_eq!(render_alerts("fr", &resolved, "u").subject, "[TAK Manager] Résolu : Le service caddy ne tourne pas (état : exited).");
        assert!(render_alerts("en", &resolved, "u").subject.starts_with("[TAK Manager] Resolved: "));

        // Everything fails at once: one message, counted in its subject, listed in its body.
        let storm = AlertBatch {
            raised: vec![down.clone(), line("tak_unreachable", "", "critical", serde_json::json!({ "what": "8089, api" })), line("directory_unreachable", "", "critical", Value::Null)],
            resolved: vec![line("backup_failed", "", "warning", Value::Null)],
            reminded: vec![line("security_updates", "", "low", serde_json::json!({ "count": 4 }))],
        };
        let mail = render_alerts("en", &storm, "u");
        assert_eq!(mail.subject, "[TAK Manager] Alerts — new: 3, resolved: 1, still open: 1");
        assert_eq!(mail.body.matches("\n  - [").count(), 5);
        assert!(mail.body.contains("Still open (reminder):\n  - [low] Security updates waiting to be installed on the server: 4."));
        assert!(!mail.subject.contains('\n') && !mail.body.contains("{{"));
    }

    #[test]
    fn the_certificate_notice_tells_leaders_what_to_do() {
        let vars = [("team", "Alpha"), ("access", "alpha-shared"), ("days", "30"), ("date", "19/12/2026 à 14:05 UTC"), ("url", "https://tak.example.org/teams/1")];
        let fr = render(Template::CertificateNotice, "fr", &vars, "https://tak.example.org");
        assert_eq!(fr.subject, "Alpha : le certificat TAK de alpha-shared expire dans 30 jours");
        assert!(fr.body.contains("Régénérer") && fr.body.contains("19/12/2026 à 14:05 UTC") && fr.body.contains("https://tak.example.org/teams/1"));
        let en = render(Template::CertificateNotice, "en", &vars, "u");
        assert!(en.body.contains("Regenerate") && en.body.contains("Nothing renews it"));
        assert!(!en.body.contains("{{") && !fr.body.contains("{{"));
    }

    #[test]
    fn subjects_stay_on_one_line() {
        let r = render(Template::TeamRequest, "en", &[("team", "A\r\nBcc: x"), ("requester", "r"), ("message", "m")], "u");
        assert!(!r.subject.contains('\n') && !r.subject.contains('\r'));
    }
}
