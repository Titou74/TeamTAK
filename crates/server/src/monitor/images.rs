//! "Is a newer image available?" for the services pulled from a registry.
//!
//! Every image of the stack is pinned, so "newer" has a precise meaning for each kind:
//!
//! - **pulled by tag** (`caddy:2.11-alpine`, `postgres:17-alpine`, `axllent/mailpit:v1.29`):
//!   the registry now serves another image under the same tag, that is a patch release or a
//!   rebuild. A newer minor or major tag is a deliberate change of the Compose file and is
//!   not looked for;
//! - **pinned by digest** (`vegardit/openldap@sha256:…`): nothing newer can exist under
//!   that reference. The registry's `latest` is compared for information only; that image
//!   is rebuilt every week, so it almost always differs;
//! - **built on this machine** (`teamtak/…`): no registry. TAK Server has its own
//!   version check.
//!
//! The registry is asked for one header per image (`HEAD` on the manifest, which registries
//! do not count as a pull), at most every twelve hours, and the answers are kept in the
//! database so a restart does not ask again.

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use teamtak_core::versions::{local_digests, parse_image, ImageSource};
use time::OffsetDateTime;

use super::snapshot::ImageUpdate;
use super::Monitor;

/// Registries are asked at most this often.
const CHECK_EVERY: time::Duration = time::Duration::hours(12);

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Check {
    /// Digest the registry serves for the reference that was asked.
    pub digest: Option<String>,
    pub checked_at: String,
    pub error: Option<String>,
}

/// Keyed by the image as the Compose file names it.
pub type Checks = BTreeMap<String, Check>;

pub async fn load_checks(db: &PgPool) -> Result<Checks, sqlx::Error> {
    let (checks,): (serde_json::Value,) = sqlx::query_as("SELECT image_checks FROM monitor_state WHERE id = 1").fetch_one(db).await?;
    Ok(serde_json::from_value(checks).unwrap_or_default())
}

/// What is known about a newer version of `image`, from the last registry answers.
pub fn status(image: &str, repo_digests: &[String], checks: &Checks) -> ImageUpdate {
    let check = checks.get(image);
    let checked_at = check.map(|c| c.checked_at.clone());
    let error = check.and_then(|c| c.error.clone());
    let remote = check.and_then(|c| c.digest.as_deref());
    match parse_image(image) {
        None => ImageUpdate { source: "unknown", status: "unknown", checked_at, error },
        Some(ImageSource::Local) => ImageUpdate { source: "local", status: "local", checked_at: None, error: None },
        Some(ImageSource::Tag { .. }) => {
            let held = local_digests(repo_digests);
            let status = match remote {
                // No digest known locally: the image cannot be compared.
                Some(_) if held.is_empty() => "unknown",
                Some(remote) if held.contains(&remote) => "up_to_date",
                Some(_) => "update_available",
                None => "unknown",
            };
            ImageUpdate { source: "tag", status, checked_at, error }
        }
        Some(ImageSource::Digest { digest, .. }) => {
            let status = match remote {
                Some(remote) if remote != digest => "pinned_newer_exists",
                _ => "pinned",
            };
            ImageUpdate { source: "digest", status, checked_at, error }
        }
    }
}

/// The parts of a `WWW-Authenticate: Bearer realm="…",service="…",scope="…"` header.
pub fn parse_bearer_challenge(header: &str) -> Option<(String, Option<String>, Option<String>)> {
    let params = header.trim().strip_prefix("Bearer ").or_else(|| header.trim().strip_prefix("bearer "))?;
    let mut found: BTreeMap<String, String> = BTreeMap::new();
    for part in params.split(',') {
        if let Some((key, value)) = part.trim().split_once('=') {
            found.insert(key.trim().to_lowercase(), value.trim().trim_matches('"').to_string());
        }
    }
    let realm = found.remove("realm").filter(|r| r.starts_with("https://"))?;
    Some((realm, found.remove("service"), found.remove("scope")))
}

const MANIFEST_TYPES: &str = "application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.list.v2+json, \
                              application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json";

/// Digest a registry serves for `repository:reference`, with the anonymous token exchange
/// public registries ask for.
async fn registry_digest(http: &reqwest::Client, registry: &str, repository: &str, reference: &str) -> Result<String, String> {
    let url = format!("https://{registry}/v2/{repository}/manifests/{reference}");
    let head = |token: Option<String>| {
        let request = http.head(&url).header("Accept", MANIFEST_TYPES);
        match token {
            Some(token) => request.bearer_auth(token),
            None => request,
        }
    };
    let mut response = head(None).send().await.map_err(|e| e.without_url().to_string())?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        let challenge = response.headers().get("www-authenticate").and_then(|v| v.to_str().ok()).and_then(parse_bearer_challenge);
        let (realm, service, scope) = challenge.ok_or("the registry asks for a login this platform does not have")?;
        let mut query: Vec<(&str, String)> = Vec::new();
        query.extend(service.map(|s| ("service", s)));
        query.push(("scope", scope.unwrap_or_else(|| format!("repository:{repository}:pull"))));
        let token: serde_json::Value = http
            .get(&realm)
            .query(&query)
            .send()
            .await
            .map_err(|e| e.without_url().to_string())?
            .json()
            .await
            .map_err(|e| e.without_url().to_string())?;
        let token = token.get("token").or_else(|| token.get("access_token")).and_then(|t| t.as_str()).ok_or("the registry gave no token")?.to_string();
        response = head(Some(token)).send().await.map_err(|e| e.without_url().to_string())?;
    }
    if !response.status().is_success() {
        return Err(format!("the registry answered {}", response.status().as_u16()));
    }
    response
        .headers()
        .get("docker-content-digest")
        .and_then(|v| v.to_str().ok())
        .filter(|d| d.starts_with("sha256:"))
        .map(str::to_string)
        .ok_or_else(|| "the registry gave no digest".to_string())
}

/// Asks the registries about the images of the stack, when the last time is old enough.
/// Runs apart from the monitoring loop, which never waits for a registry.
pub fn refresh_if_due(monitor: &Arc<Monitor>) {
    if monitor.images_running.swap(true, Ordering::SeqCst) {
        return;
    }
    let monitor = monitor.clone();
    tokio::spawn(async move {
        if let Err(e) = refresh(&monitor).await {
            tracing::warn!(error = %format!("{e:#}"), "image update check");
        }
        monitor.images_running.store(false, Ordering::SeqCst);
    });
}

async fn refresh(monitor: &Monitor) -> anyhow::Result<()> {
    let (checked_at,): (Option<OffsetDateTime>,) = sqlx::query_as("SELECT image_checked_at FROM monitor_state WHERE id = 1").fetch_one(&monitor.db).await?;
    let now = OffsetDateTime::now_utc();
    if checked_at.is_some_and(|at| now - at < CHECK_EVERY) {
        return Ok(());
    }
    let (Some(http), Some(snapshot)) = (&monitor.http, monitor.latest()) else { return Ok(()) };
    let mut images: Vec<String> = snapshot.services.services.iter().filter(|s| s.present).map(|s| s.image.clone()).collect();
    images.sort();
    images.dedup();
    if images.is_empty() {
        // The agent did not answer: nothing to ask about, try again at the next look.
        return Ok(());
    }
    let stamp = now.format(&time::format_description::well_known::Rfc3339).unwrap_or_default();
    let mut checks = Checks::new();
    for image in images {
        let (registry, repository, reference) = match parse_image(&image) {
            Some(ImageSource::Tag { registry, repository, tag }) => (registry, repository, tag),
            Some(ImageSource::Digest { registry, repository, .. }) => (registry, repository, "latest".to_string()),
            _ => continue,
        };
        let check = match registry_digest(http, &registry, &repository, &reference).await {
            Ok(digest) => Check { digest: Some(digest), checked_at: stamp.clone(), error: None },
            Err(error) => Check { digest: None, checked_at: stamp.clone(), error: Some(error) },
        };
        tracing::info!(image, digest = ?check.digest, error = ?check.error, "registry asked");
        checks.insert(image, check);
    }
    // Written even when a registry failed: the next attempt is in twelve hours, not in a minute.
    sqlx::query("UPDATE monitor_state SET image_checks = $1, image_checked_at = $2 WHERE id = 1")
        .bind(serde_json::json!(checks))
        .bind(now)
        .execute(&monitor.db)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checks(image: &str, digest: Option<&str>) -> Checks {
        [(image.to_string(), Check { digest: digest.map(str::to_string), checked_at: "2026-11-19T03:00:00Z".into(), error: None })].into()
    }

    #[test]
    fn an_image_pulled_by_tag_is_compared_with_what_the_registry_serves_under_that_tag() {
        let held = vec!["caddy@sha256:aaa".to_string()];
        let same = status("caddy:2.11-alpine", &held, &checks("caddy:2.11-alpine", Some("sha256:aaa")));
        assert_eq!((same.source, same.status), ("tag", "up_to_date"));
        let moved = status("caddy:2.11-alpine", &held, &checks("caddy:2.11-alpine", Some("sha256:bbb")));
        assert_eq!(moved.status, "update_available");
        assert_eq!(moved.checked_at.as_deref(), Some("2026-11-19T03:00:00Z"));
        // Never asked, or the registry did not answer: unknown, not "up to date".
        assert_eq!(status("caddy:2.11-alpine", &held, &Checks::new()).status, "unknown");
        assert_eq!(status("caddy:2.11-alpine", &held, &checks("caddy:2.11-alpine", None)).status, "unknown");
        assert_eq!(status("caddy:2.11-alpine", &[], &checks("caddy:2.11-alpine", Some("sha256:aaa"))).status, "unknown");
    }

    #[test]
    fn an_image_pinned_by_digest_is_never_an_update_only_information() {
        let image = "vegardit/openldap@sha256:f6e7";
        let pinned = status(image, &[], &checks(image, Some("sha256:f6e7")));
        assert_eq!((pinned.source, pinned.status), ("digest", "pinned"));
        assert_eq!(status(image, &[], &checks(image, Some("sha256:0000"))).status, "pinned_newer_exists");
        assert_eq!(status(image, &[], &Checks::new()).status, "pinned");
    }

    #[test]
    fn images_built_here_have_no_registry() {
        // Docker gives them a digest too; the name tells them apart.
        let update = status("teamtak/takserver:5.8-RELEASE-103", &["teamtak/takserver@sha256:6857".to_string()], &Checks::new());
        assert_eq!((update.source, update.status, update.checked_at), ("local", "local", None));
    }

    #[test]
    fn registry_login_challenges_are_read() {
        let hub = r#"Bearer realm="https://auth.docker.io/token",service="registry.docker.io",scope="repository:library/caddy:pull""#;
        assert_eq!(
            parse_bearer_challenge(hub),
            Some(("https://auth.docker.io/token".into(), Some("registry.docker.io".into()), Some("repository:library/caddy:pull".into())))
        );
        assert_eq!(parse_bearer_challenge(r#"Bearer realm="https://ghcr.io/token""#), Some(("https://ghcr.io/token".into(), None, None)));
        // A token is only ever asked over HTTPS, and only for a Bearer challenge.
        assert_eq!(parse_bearer_challenge(r#"Bearer realm="http://auth.example/token""#), None);
        assert_eq!(parse_bearer_challenge(r#"Basic realm="registry""#), None);
    }
}
