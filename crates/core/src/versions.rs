//! Versions of TAK Server and names of container images, for the "is it up to date?" part
//! of the monitoring.

/// A TAK Server version as tak.gov names it: `5.8-RELEASE-103`. The running server appends
/// a build tag (`5.8-RELEASE-103-HEAD`) and its API writes the same version another way
/// (`5.8.103-RELEASE`); all three give the same numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TakVersion {
    pub major: u32,
    pub minor: u32,
    pub release: u32,
}

impl std::fmt::Display for TakVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}-RELEASE-{}", self.major, self.minor, self.release)
    }
}

/// Reads a TAK Server version in any of the forms the server and its ZIP use.
pub fn parse_tak_version(text: &str) -> Option<TakVersion> {
    // Every run of digits, in order: "5.8-RELEASE-103-HEAD" and "5.8.103-RELEASE" both give
    // 5, 8 and 103.
    let numbers: Vec<u32> = text
        .trim()
        .split(|c: char| !c.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .map(|part| part.parse().ok())
        .collect::<Option<_>>()?;
    match numbers[..] {
        [major, minor, release] => Some(TakVersion { major, minor, release }),
        _ => None,
    }
}

/// Where an image comes from, which decides what "a newer one exists" can mean.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageSource {
    /// Built on this machine by install.sh (`tak-manager/…`): no registry to compare with.
    Local,
    /// Pulled by tag: newer means the registry now serves another image under that tag.
    Tag { registry: String, repository: String, tag: String },
    /// Pinned by digest on purpose: nothing newer can exist under that reference. The
    /// registry's `latest` is compared for information only.
    Digest { registry: String, repository: String, digest: String },
}

const DOCKER_HUB: &str = "registry-1.docker.io";

/// Reads an image reference as written in the Compose file.
pub fn parse_image(reference: &str) -> Option<ImageSource> {
    let reference = reference.trim();
    if reference.is_empty() || reference.contains(char::is_whitespace) {
        return None;
    }
    if reference.starts_with("tak-manager/") {
        return Some(ImageSource::Local);
    }
    let (name, digest) = match reference.split_once('@') {
        Some((name, digest)) => (name, Some(digest)),
        None => (reference, None),
    };
    // A registry is a first path element that looks like a host: it has a dot or a port.
    let (registry, path) = match name.split_once('/') {
        Some((first, rest)) if first.contains('.') || first.contains(':') || first == "localhost" => (first.to_string(), rest.to_string()),
        _ => (DOCKER_HUB.to_string(), name.to_string()),
    };
    let (path, tag) = match path.rsplit_once(':') {
        Some((path, tag)) if !tag.contains('/') => (path.to_string(), Some(tag.to_string())),
        _ => (path, None),
    };
    // Docker Hub's official images live under "library/".
    let repository = if registry == DOCKER_HUB && !path.contains('/') { format!("library/{path}") } else { path };
    if repository.is_empty() {
        return None;
    }
    Some(match digest {
        Some(digest) if digest.starts_with("sha256:") => ImageSource::Digest { registry, repository, digest: digest.to_string() },
        Some(_) => return None,
        None => ImageSource::Tag { registry, repository, tag: tag.unwrap_or_else(|| "latest".to_string()) },
    })
}

/// The digest under which this machine holds an image, among the `repository@sha256:…`
/// entries Docker lists for it.
pub fn local_digests(repo_digests: &[String]) -> Vec<&str> {
    repo_digests.iter().filter_map(|entry| entry.split_once('@').map(|(_, digest)| digest)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tak_versions_are_read_in_every_form_the_server_uses() {
        let v = TakVersion { major: 5, minor: 8, release: 103 };
        // ZIP name and deploy/.env; GET /Marti/api/version; GET /Marti/api/version/config.
        for text in ["5.8-RELEASE-103", "5.8-RELEASE-103-HEAD", "5.8.103-RELEASE", " 5.8-RELEASE-103\n"] {
            assert_eq!(parse_tak_version(text), Some(v), "{text:?}");
        }
        assert_eq!(v.to_string(), "5.8-RELEASE-103");
        for text in ["", "latest", "5.8", "5.8-RELEASE", "1.2.3.4", "99999999999.1.1"] {
            assert_eq!(parse_tak_version(text), None, "{text:?}");
        }
    }

    #[test]
    fn tak_versions_are_ordered() {
        let v = |s: &str| parse_tak_version(s).unwrap();
        assert!(v("5.8-RELEASE-103") < v("5.8-RELEASE-104"));
        assert!(v("5.8-RELEASE-103") < v("5.9-RELEASE-2"));
        assert!(v("5.10-RELEASE-1") > v("5.9-RELEASE-80"), "numbers, not text");
        assert!(v("6.0-RELEASE-1") > v("5.99-RELEASE-999"));
    }

    #[test]
    fn images_of_the_stack_are_classified() {
        let tag = |registry: &str, repository: &str, tag: &str| ImageSource::Tag { registry: registry.into(), repository: repository.into(), tag: tag.into() };
        assert_eq!(parse_image("caddy:2.11-alpine"), Some(tag(DOCKER_HUB, "library/caddy", "2.11-alpine")));
        assert_eq!(parse_image("postgres:17-alpine"), Some(tag(DOCKER_HUB, "library/postgres", "17-alpine")));
        assert_eq!(parse_image("axllent/mailpit:v1.29"), Some(tag(DOCKER_HUB, "axllent/mailpit", "v1.29")));
        assert_eq!(parse_image("debian"), Some(tag(DOCKER_HUB, "library/debian", "latest")));
        assert_eq!(parse_image("ghcr.io/owner/app:1.2"), Some(tag("ghcr.io", "owner/app", "1.2")));
        assert_eq!(parse_image("localhost:5000/app"), Some(tag("localhost:5000", "app", "latest")));
        assert_eq!(
            parse_image("vegardit/openldap@sha256:f6e74dd8"),
            Some(ImageSource::Digest { registry: DOCKER_HUB.into(), repository: "vegardit/openldap".into(), digest: "sha256:f6e74dd8".into() })
        );
        for local in ["tak-manager/app:local", "tak-manager/takserver:5.8-RELEASE-103", "tak-manager/agent:local"] {
            assert_eq!(parse_image(local), Some(ImageSource::Local), "{local}");
        }
        assert_eq!(parse_image(""), None);
        assert_eq!(parse_image("caddy@md5:abc"), None);
    }

    #[test]
    fn local_digests_come_from_the_repo_digest_entries() {
        let entries = vec!["caddy@sha256:aa".to_string(), "mirror.example/caddy@sha256:bb".to_string(), "odd".to_string()];
        assert_eq!(local_digests(&entries), vec!["sha256:aa", "sha256:bb"]);
    }
}
