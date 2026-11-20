//! "Is a newer TeamTAK published?": the latest release of the GitHub repository TeamTAK is
//! released from (`RELEASE_REPOSITORY`), compared with the version that runs.
//!
//! GitHub is asked at most every twelve hours, anonymously, for one small document:
//! `GET /repos/<owner>/<name>/releases/latest`, which leaves drafts and pre-releases out. The
//! answer is kept in the database so a restart does not ask again. It is asked every hour
//! while it does not answer, and while the version that runs is newer than its answer: that
//! version is published since, or about to be (the platform is often updated before its
//! release is published), and the answer is behind. A version that starts newer than the
//! answer has it asked at once, and an administrator can too.
//! Nothing is ever downloaded or installed: the administrator updates over SSH.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use teamtak_core::alerts::ReleaseObservation;
use teamtak_core::versions::{parse_release_version, release_is_newer};
use time::OffsetDateTime;

use super::snapshot::{LatestRelease, ReleaseView};
use super::Monitor;

/// GitHub is asked at most this often…
const CHECK_EVERY: time::Duration = time::Duration::hours(12);
/// …or this often while it does not answer, or its answer is older than what runs.
const RETRY_EVERY: time::Duration = time::Duration::hours(1);

/// The version that runs.
pub const INSTALLED: &str = env!("CARGO_PKG_VERSION");

/// A release as GitHub describes it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Release {
    pub tag: String,
    pub name: String,
    pub url: String,
    pub published_at: Option<String>,
}

/// What is kept between two questions.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Check {
    /// The repository asked, `owner/name`: an answer about another one is not used.
    pub repository: String,
    /// When the repository last answered, whether or not it had a release. `None` until
    /// it answered once.
    pub answered_at: Option<String>,
    /// The latest release in that answer; `None` when the repository has none yet.
    pub latest: Option<Release>,
    pub checked_at: Option<String>,
    /// Why the last question failed; the answer before it is kept.
    pub error: Option<String>,
}

pub async fn load(db: &PgPool) -> Result<Check, sqlx::Error> {
    let (check,): (serde_json::Value,) =
        sqlx::query_as("SELECT release_check FROM monitor_state WHERE id = 1").fetch_one(db).await?;
    Ok(serde_json::from_value(check).unwrap_or_default())
}

/// What the dashboard shows, and what the alert rules see, from the last answer.
pub fn status(repository: Option<&str>, check: &Check) -> (ReleaseView, Option<ReleaseObservation>) {
    let Some(repository) = repository else {
        let view =
            ReleaseView { installed: INSTALLED, repository: None, status: "off", latest: None, checked_at: None, error: None };
        // Turned off on purpose: nothing to say, and an open alert resolves.
        return (view, Some(ReleaseObservation { installed: INSTALLED.to_string(), latest: None }));
    };
    let check = Some(check).filter(|c| c.repository == repository);
    let answered = check.is_some_and(|c| c.answered_at.is_some());
    let latest = check.and_then(|c| c.latest.as_ref());
    let status = match latest {
        _ if !answered => "unknown",
        None => "no_release",
        Some(release) if release_is_newer(&release.tag, INSTALLED) => "update_available",
        Some(_) => "up_to_date",
    };
    let view = ReleaseView {
        installed: INSTALLED,
        repository: Some(repository.to_string()),
        status,
        latest: latest.map(|release| LatestRelease {
            version: parse_release_version(&release.tag).map(|v| v.to_string()).unwrap_or_else(|| release.tag.clone()),
            tag: release.tag.clone(),
            name: release.name.clone(),
            url: release.url.clone(),
            published_at: release.published_at.clone(),
        }),
        checked_at: check.and_then(|c| c.checked_at.clone()),
        error: check.and_then(|c| c.error.clone()),
    };
    let observation =
        answered.then(|| ReleaseObservation { installed: INSTALLED.to_string(), latest: latest.map(|r| r.tag.clone()) });
    (view, observation)
}

/// The fields of GitHub's answer that are used.
#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    published_at: Option<String>,
}

/// Reads GitHub's description of a release. The link is only kept when it leads to GitHub.
fn read_release(repository: &str, body: &str) -> Result<Release, String> {
    let release: GithubRelease = serde_json::from_str(body).map_err(|e| format!("GitHub's answer could not be read: {e}"))?;
    let url = if release.html_url.starts_with("https://github.com/") {
        release.html_url
    } else {
        format!("https://github.com/{repository}/releases")
    };
    Ok(Release {
        name: release.name.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| release.tag_name.clone()),
        tag: release.tag_name,
        url,
        published_at: release.published_at,
    })
}

/// The latest release of `repository`; `None` when it has none.
async fn latest_release(http: &reqwest::Client, repository: &str) -> Result<Option<Release>, String> {
    let response = http
        .get(format!("https://api.github.com/repos/{repository}/releases/latest"))
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
        .map_err(|e| e.without_url().to_string())?;
    match response.status().as_u16() {
        200 => read_release(repository, &response.text().await.map_err(|e| e.without_url().to_string())?).map(Some),
        // No release published yet (or no such repository, which the address would show).
        404 => Ok(None),
        403 | 429 => Err("GitHub limits how often it may be asked from this address; it will be asked again later".to_string()),
        code => Err(format!("GitHub answered {code}")),
    }
}

/// Asks GitHub when the last time is old enough. Runs apart from the monitoring loop,
/// which never waits for GitHub.
pub fn refresh_if_due(monitor: &Arc<Monitor>) {
    if monitor.targets.release_repository.is_none() || monitor.release_running.swap(true, Ordering::SeqCst) {
        return;
    }
    let monitor = monitor.clone();
    tokio::spawn(async move {
        if let Err(e) = refresh(&monitor).await {
            tracing::warn!(error = %format!("{e:#}"), "release check");
        }
        monitor.release_running.store(false, Ordering::SeqCst);
    });
}

/// Whether GitHub is asked now. `previous`: the last answer about the repository asked,
/// empty when there is none; `checked_at`: when GitHub was last asked; `started_at`: when the
/// version that runs started.
fn is_due(previous: &Check, checked_at: Option<OffsetDateTime>, started_at: OffsetDateTime, now: OffsetDateTime) -> bool {
    // Never asked, or asked about another repository than this one: at once.
    let Some(checked_at) = checked_at.filter(|_| !previous.repository.is_empty()) else { return true };
    // The version that runs is newer than GitHub's latest release: it was published since,
    // or is about to be. Asked at once when that version started after the answer (the
    // platform was just updated), then every hour until GitHub has it.
    let behind = previous.latest.as_ref().is_some_and(|latest| release_is_newer(INSTALLED, &latest.tag));
    if behind && checked_at < started_at {
        return true;
    }
    let wait = if previous.error.is_some() || behind { RETRY_EVERY } else { CHECK_EVERY };
    now - checked_at >= wait
}

/// The last answer about the repository asked now; empty when there is none.
async fn previous(monitor: &Monitor, repository: &str) -> Result<Check, sqlx::Error> {
    Ok(Some(load(&monitor.db).await?).filter(|c| c.repository == repository).unwrap_or_default())
}

async fn refresh(monitor: &Monitor) -> anyhow::Result<()> {
    let (Some(repository), Some(_)) = (monitor.targets.release_repository.as_deref(), &monitor.http) else { return Ok(()) };
    let (checked_at,): (Option<OffsetDateTime>,) =
        sqlx::query_as("SELECT release_checked_at FROM monitor_state WHERE id = 1").fetch_one(&monitor.db).await?;
    let previous = previous(monitor, repository).await?;
    if !is_due(&previous, checked_at, monitor.started_at, OffsetDateTime::now_utc()) {
        return Ok(());
    }
    ask(monitor, repository, previous).await
}

/// Asks GitHub now, however recently it was asked: an administrator's "check again". Does
/// nothing when the check is turned off. A refusal of GitHub is kept with the answer, as
/// for a scheduled check.
pub async fn check_now(monitor: &Monitor) -> anyhow::Result<()> {
    let Some(repository) = monitor.targets.release_repository.as_deref() else { return Ok(()) };
    let previous = previous(monitor, repository).await?;
    ask(monitor, repository, previous).await
}

/// Asks GitHub and keeps its answer, or why it did not give one beside the previous answer.
async fn ask(monitor: &Monitor, repository: &str, previous: Check) -> anyhow::Result<()> {
    let Some(http) = &monitor.http else { anyhow::bail!("no HTTPS client to ask GitHub with") };
    let now = OffsetDateTime::now_utc();
    let stamp = now.format(&time::format_description::well_known::Rfc3339).ok();
    let check = match latest_release(http, repository).await {
        Ok(latest) => {
            tracing::info!(repository, latest = ?latest.as_ref().map(|r| &r.tag), installed = INSTALLED, "GitHub asked for the latest release");
            Check { repository: repository.to_string(), answered_at: stamp.clone(), latest, checked_at: stamp, error: None }
        }
        Err(error) => {
            tracing::warn!(repository, error, "GitHub did not say which release is the latest");
            Check { repository: repository.to_string(), checked_at: stamp, error: Some(error), ..previous }
        }
    };
    sqlx::query("UPDATE monitor_state SET release_check = $1, release_checked_at = $2 WHERE id = 1")
        .bind(serde_json::json!(check))
        .bind(now)
        .execute(&monitor.db)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPO: &str = "Titou74/TeamTAK";

    fn answered(latest: Option<&str>) -> Check {
        Check {
            repository: REPO.into(),
            answered_at: Some("2026-11-20T03:00:00Z".into()),
            latest: latest.map(|tag| Release {
                tag: tag.into(),
                name: format!("TeamTAK {tag}"),
                url: format!("https://github.com/{REPO}/releases/tag/{tag}"),
                published_at: None,
            }),
            checked_at: Some("2026-11-20T03:00:00Z".into()),
            error: None,
        }
    }

    /// A version after the one that runs, whatever it is.
    fn next_version() -> String {
        let v = parse_release_version(INSTALLED).expect("Cargo.toml's version is a release");
        format!("v{}.{}.{}", v.major, v.minor, v.patch + 1)
    }

    #[test]
    fn a_newer_release_is_an_update_and_the_same_one_is_not() {
        let next = next_version();
        let (view, observation) = status(Some(REPO), &answered(Some(&next)));
        assert_eq!(view.status, "update_available");
        let latest = view.latest.unwrap();
        assert_eq!(format!("v{}", latest.version), next, "shown without the tag's v");
        assert_eq!(observation, Some(ReleaseObservation { installed: INSTALLED.into(), latest: Some(next.clone()) }));

        let same = format!("v{INSTALLED}");
        assert_eq!(status(Some(REPO), &answered(Some(&same))).0.status, "up_to_date");
        assert_eq!(status(Some(REPO), &answered(Some("v0.0.1"))).0.status, "up_to_date", "older than what runs");
        let (view, observation) = status(Some(REPO), &answered(None));
        assert_eq!(view.status, "no_release");
        assert_eq!(observation, Some(ReleaseObservation { installed: INSTALLED.into(), latest: None }));
    }

    #[test]
    fn nothing_is_judged_before_github_answered_once() {
        let (view, observation) = status(Some(REPO), &Check::default());
        assert_eq!((view.status, observation), ("unknown", None));
        // A failure after an answer keeps that answer, with the error beside it.
        let mut failed = answered(Some(&next_version()));
        failed.error = Some("GitHub answered 502".into());
        let (view, observation) = status(Some(REPO), &failed);
        assert_eq!((view.status, view.error.as_deref()), ("update_available", Some("GitHub answered 502")));
        assert!(observation.is_some());
        // Never answered at all.
        let never = Check {
            repository: REPO.into(),
            checked_at: Some("2026-11-20T03:00:00Z".into()),
            error: Some("timed out".into()),
            ..Default::default()
        };
        assert_eq!(status(Some(REPO), &never).1, None);
        // An answer about another repository (RELEASE_REPOSITORY changed) is not used.
        let (view, observation) = status(Some("someone/fork"), &answered(Some(&next_version())));
        assert_eq!((view.status, view.latest.is_none(), observation), ("unknown", true, None));
    }

    #[test]
    fn turned_off_says_so_and_resolves_the_alert() {
        let (view, observation) = status(None, &answered(Some(&next_version())));
        assert_eq!((view.status, view.repository, view.latest.is_none()), ("off", None, true));
        assert_eq!(observation, Some(ReleaseObservation { installed: INSTALLED.into(), latest: None }));
    }

    #[test]
    fn github_is_asked_when_the_answer_is_old_or_stale() {
        let at = |text: &str| OffsetDateTime::parse(text, &time::format_description::well_known::Rfc3339).unwrap();
        let asked = at("2026-11-20T03:00:00Z");
        let started = at("2026-11-20T02:00:00Z");
        let due = |check: &Check, checked_at, started_at, now: &str| is_due(check, checked_at, started_at, at(now));
        let current = answered(Some(&format!("v{INSTALLED}")));

        // Never asked, or about another repository: at once.
        assert!(due(&Check::default(), None, started, "2026-11-20T03:01:00Z"));
        assert!(due(&Check::default(), Some(asked), started, "2026-11-20T03:01:00Z"));
        // Asked lately: twelve hours later, or one after a failure.
        assert!(!due(&current, Some(asked), started, "2026-11-20T14:59:00Z"));
        assert!(due(&current, Some(asked), started, "2026-11-20T15:00:00Z"));
        let mut failed = current.clone();
        failed.error = Some("GitHub answered 502".into());
        assert!(!due(&failed, Some(asked), started, "2026-11-20T03:59:00Z"));
        assert!(due(&failed, Some(asked), started, "2026-11-20T04:00:00Z"));

        // The version that runs is newer than the answer, given before it started: the
        // platform was updated since, and GitHub is asked again at once…
        let older = answered(Some("v0.0.1"));
        let updated = at("2026-11-20T04:00:00Z");
        assert!(due(&older, Some(asked), updated, "2026-11-20T04:01:00Z"));
        // …and, once asked after the start, every hour while GitHub has nothing as recent
        // (the platform updated before its release is published).
        assert!(!due(&older, Some(asked), started, "2026-11-20T03:59:00Z"));
        assert!(due(&older, Some(asked), started, "2026-11-20T04:00:00Z"));
        // An answer as recent as what runs is not behind.
        assert!(!due(&current, Some(asked), updated, "2026-11-20T04:01:00Z"));
        assert!(!due(&answered(None), Some(asked), updated, "2026-11-20T04:01:00Z"));
    }

    #[test]
    fn githubs_answer_is_read() {
        // Trimmed from GET https://api.github.com/repos/<owner>/<name>/releases/latest.
        let body = r#"{
            "url": "https://api.github.com/repos/Titou74/TeamTAK/releases/1",
            "html_url": "https://github.com/Titou74/TeamTAK/releases/tag/v1.0.1",
            "tag_name": "v1.0.1", "target_commitish": "main", "name": "TeamTAK 1.0.1",
            "draft": false, "prerelease": false,
            "created_at": "2026-11-20T15:00:00Z", "published_at": "2026-11-20T15:02:00Z",
            "assets": [], "body": "Notes"
        }"#;
        assert_eq!(
            read_release(REPO, body).unwrap(),
            Release {
                tag: "v1.0.1".into(),
                name: "TeamTAK 1.0.1".into(),
                url: "https://github.com/Titou74/TeamTAK/releases/tag/v1.0.1".into(),
                published_at: Some("2026-11-20T15:02:00Z".into()),
            }
        );
        // No title, and a link elsewhere than GitHub: the tag names it, the releases page is linked.
        let odd = read_release(REPO, r#"{ "tag_name": "v1.0.2", "name": "", "html_url": "https://example.org/x" }"#).unwrap();
        assert_eq!((odd.name.as_str(), odd.url.as_str()), ("v1.0.2", "https://github.com/Titou74/TeamTAK/releases"));
        assert!(read_release(REPO, "<html>").is_err());
    }
}
