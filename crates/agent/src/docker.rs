//! The agent's only way to Docker: the `docker` command line, through the socket mounted
//! in its container. Everything it asks is limited to the containers of one Compose
//! project, recognised by the labels Compose sets, never by a name coming from a request.

use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tokio::io::AsyncWriteExt;

use crate::parse;
use crate::report::ContainerReport;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DockerError {
    /// The command could not be started, or Docker did not answer in time.
    Unavailable(String),
    /// Docker answered with an error.
    Failed(String),
}

impl std::fmt::Display for DockerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DockerError::Unavailable(e) => write!(f, "Docker unavailable: {e}"),
            DockerError::Failed(e) => write!(f, "Docker refused: {e}"),
        }
    }
}

impl std::error::Error for DockerError {}

#[async_trait]
pub trait Docker: Send + Sync {
    /// Runs `docker <args>` and returns what it printed.
    async fn run(&self, args: &[&str], stdin: Option<&[u8]>, timeout: Duration) -> Result<String, DockerError>;

    /// Like [`run`], for a command whose partial answer is still useful: what it printed,
    /// and whether it succeeded. `docker image inspect a b c` prints what it found and then
    /// fails when one of the three is gone.
    async fn run_partial(&self, args: &[&str], timeout: Duration) -> Result<(bool, String), DockerError> {
        match self.run(args, None, timeout).await {
            Ok(output) => Ok((true, output)),
            Err(DockerError::Failed(_)) => Ok((false, String::new())),
            Err(unavailable) => Err(unavailable),
        }
    }
}

/// The real thing.
pub struct DockerCli;

impl DockerCli {
    /// Runs the command and returns whether it succeeded, its output and its error output.
    async fn execute(args: &[&str], stdin: Option<&[u8]>, timeout: Duration) -> Result<(bool, String, String), DockerError> {
        let mut command = tokio::process::Command::new("docker");
        command
            .args(args)
            .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|e| DockerError::Unavailable(format!("starting docker: {e}")))?;
        if let (Some(input), Some(mut pipe)) = (stdin, child.stdin.take()) {
            pipe.write_all(input).await.map_err(|e| DockerError::Unavailable(format!("writing to docker: {e}")))?;
            drop(pipe);
        }
        let output = tokio::time::timeout(timeout, child.wait_with_output())
            .await
            .map_err(|_| DockerError::Unavailable(format!("no answer after {}s", timeout.as_secs())))?
            .map_err(|e| DockerError::Unavailable(e.to_string()))?;
        let printed = String::from_utf8_lossy(&output.stdout).into_owned();
        let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
        // The daemon being down is a different thing from a refused command.
        if !output.status.success() && (message.contains("Cannot connect to the Docker daemon") || message.contains("permission denied while trying to connect")) {
            return Err(DockerError::Unavailable(message));
        }
        Ok((output.status.success(), printed, message))
    }
}

#[async_trait]
impl Docker for DockerCli {
    async fn run(&self, args: &[&str], stdin: Option<&[u8]>, timeout: Duration) -> Result<String, DockerError> {
        match Self::execute(args, stdin, timeout).await? {
            (true, printed, _) => Ok(printed),
            (false, _, message) => Err(DockerError::Failed(message)),
        }
    }

    async fn run_partial(&self, args: &[&str], timeout: Duration) -> Result<(bool, String), DockerError> {
        let (ok, printed, _) = Self::execute(args, None, timeout).await?;
        Ok((ok, printed))
    }
}

const QUICK: Duration = Duration::from_secs(15);

fn project_filter(project: &str) -> String {
    format!("label=com.docker.compose.project={project}")
}

/// A container identifier as Docker prints it: 12 to 64 hexadecimal characters.
fn is_container_id(s: &str) -> bool {
    (12..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Every container of the Compose project, running or not, as `docker inspect` describes it.
pub async fn project_containers(docker: &dyn Docker, project: &str) -> Result<Vec<Value>, DockerError> {
    let filter = project_filter(project);
    let listed = docker.run(&["ps", "-a", "-q", "--no-trunc", "--filter", &filter], None, QUICK).await?;
    let ids: Vec<&str> = listed.lines().map(str::trim).filter(|id| is_container_id(id)).collect();
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut args = vec!["inspect"];
    args.extend(ids);
    let described = docker.run(&args, None, QUICK).await?;
    let items: Vec<Value> = serde_json::from_str(&described).map_err(|e| DockerError::Failed(format!("docker inspect: {e}")))?;
    // Belt and braces: the filter already selected the project; never trust it alone.
    Ok(items.into_iter().filter(|v| parse::compose_project(v) == Some(project)).collect())
}

/// The containers of the project with what is known of their images.
pub async fn container_reports(docker: &dyn Docker, project: &str) -> Result<Vec<ContainerReport>, DockerError> {
    let mut reports: Vec<ContainerReport> = project_containers(docker, project).await?.iter().filter_map(parse::container).collect();
    let mut image_ids: Vec<&str> = reports.iter().map(|c| c.image_id.as_str()).filter(|id| !id.is_empty()).collect();
    image_ids.sort_unstable();
    image_ids.dedup();
    let mut known: Vec<(String, Option<String>, Vec<String>)> = Vec::new();
    if !image_ids.is_empty() {
        let mut args = vec!["image", "inspect"];
        args.extend(image_ids);
        // One image that is gone must not hide the others: what was found is kept.
        if let Ok((_, text)) = docker.run_partial(&args, QUICK).await {
            let images: Vec<Value> = serde_json::from_str(&text).unwrap_or_default();
            known = images.iter().filter_map(parse::image).collect();
        }
    }
    for report in &mut reports {
        let mut found = known.iter().find(|(id, _, _)| *id == report.image_id).cloned();
        // The image a container was started from can be gone under that identifier while the
        // same image still exists under its name (it was built or pulled again, unchanged):
        // the name is then what is known of it.
        if found.is_none() && is_image_reference(&report.image) {
            if let Ok((_, text)) = docker.run_partial(&["image", "inspect", &report.image], QUICK).await {
                found = serde_json::from_str::<Vec<Value>>(&text).unwrap_or_default().first().and_then(parse::image);
            }
        }
        if let Some((_, created, digests)) = found {
            report.image_created = created;
            report.repo_digests = digests;
        }
    }
    reports.sort_by(|a, b| a.service.cmp(&b.service));
    Ok(reports)
}

/// An image name as a Compose file writes it, safe to hand to `docker image inspect`: it
/// comes from Docker's own description of a container, and is still checked before use.
fn is_image_reference(reference: &str) -> bool {
    !reference.is_empty()
        && !reference.starts_with('-')
        && reference.len() <= 300
        && reference.chars().all(|c| c.is_ascii_alphanumeric() || "._-/:@".contains(c))
}

/// Identifier of the one container that runs `service` in the project. `None` when the
/// project has no such container.
pub async fn service_container(docker: &dyn Docker, project: &str, service: &str) -> Result<Option<String>, DockerError> {
    for item in project_containers(docker, project).await? {
        let Some(report) = parse::container(&item) else { continue };
        if report.service == service {
            return Ok(item.get("Id").and_then(Value::as_str).filter(|id| is_container_id(id)).map(str::to_string));
        }
    }
    Ok(None)
}

#[cfg(test)]
pub mod fake {
    //! A Docker that answers from a script and records what it was asked.

    use std::sync::Mutex;

    use super::*;
    use serde_json::json;

    pub const TAK_ID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    pub const CADDY_ID: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    /// A container of another project on the same Docker.
    pub const FOREIGN_ID: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

    #[derive(Default)]
    pub struct FakeDocker {
        pub calls: Mutex<Vec<Vec<String>>>,
        /// Makes `ps` list the foreign container too, as if the label filter had failed.
        pub leaky_filter: bool,
        pub down: bool,
        /// The image Caddy's container was started from is gone under its identifier, and
        /// still there under its name.
        pub image_gone_by_id: bool,
    }

    fn inspected(id: &str, project: &str, service: &str, name: &str, image: &str) -> Value {
        json!({
            "Id": id, "Name": format!("/{name}"), "RestartCount": 0, "Image": format!("sha256:{service}"),
            "State": {"Status": "running", "ExitCode": 0, "StartedAt": "2026-11-19T17:13:02Z"},
            "Config": {"Image": image, "Labels": {"com.docker.compose.project": project, "com.docker.compose.service": service}}
        })
    }

    impl FakeDocker {
        pub fn calls(&self) -> Vec<Vec<String>> {
            self.calls.lock().unwrap().clone()
        }

        /// Calls that change something: everything but listing and describing.
        pub fn actions(&self) -> Vec<Vec<String>> {
            self.calls().into_iter().filter(|c| !matches!(c[0].as_str(), "ps" | "inspect" | "image" | "top")).collect()
        }
    }

    #[async_trait]
    impl Docker for FakeDocker {
        async fn run_partial(&self, args: &[&str], timeout: Duration) -> Result<(bool, String), DockerError> {
            match self.run(args, None, timeout).await {
                Ok(output) => Ok((true, output)),
                Err(DockerError::Failed(message)) => Ok((false, message.strip_prefix("PARTIAL").unwrap_or("").to_string())),
                Err(unavailable) => Err(unavailable),
            }
        }

        async fn run(&self, args: &[&str], _stdin: Option<&[u8]>, _timeout: Duration) -> Result<String, DockerError> {
            self.calls.lock().unwrap().push(args.iter().map(|a| a.to_string()).collect());
            if self.down {
                return Err(DockerError::Unavailable("Cannot connect to the Docker daemon".into()));
            }
            match args[0] {
                "ps" => {
                    assert!(args.contains(&"label=com.docker.compose.project=tak-manager"), "listing must be limited to the project: {args:?}");
                    let mut ids = vec![TAK_ID, CADDY_ID];
                    if self.leaky_filter {
                        ids.push(FOREIGN_ID);
                    }
                    Ok(ids.join("\n"))
                }
                "inspect" => {
                    let all = [
                        inspected(TAK_ID, "tak-manager", "takserver", "tak-manager-takserver-1", "tak-manager/takserver:5.8-RELEASE-103"),
                        inspected(CADDY_ID, "tak-manager", "caddy", "tak-manager-caddy-1", "caddy:2.11-alpine"),
                        inspected(FOREIGN_ID, "mapping", "caddy", "mapping-caddy-1", "caddy:2"),
                    ];
                    let asked: Vec<Value> = all.into_iter().filter(|v| args[1..].contains(&v["Id"].as_str().unwrap())).collect();
                    Ok(serde_json::to_string(&asked).unwrap())
                }
                "image" => {
                    let caddy = json!({"Id": "sha256:caddy", "Created": "2026-10-01T00:00:00Z", "RepoDigests": ["caddy@sha256:1234"]});
                    let takserver = json!({"Id": "sha256:takserver", "Created": "2026-11-19T16:00:00Z", "RepoDigests": []});
                    let by_name = args[2..] == ["caddy:2.11-alpine"];
                    if by_name {
                        Ok(json!([{"Id": "sha256:caddy-again", "Created": "2026-10-01T00:00:00Z", "RepoDigests": ["caddy@sha256:1234"]}]).to_string())
                    } else if self.image_gone_by_id {
                        // What Docker does: prints what it found, then fails.
                        Err(DockerError::Failed(format!("PARTIAL{}", json!([takserver]))))
                    } else {
                        Ok(json!([caddy, takserver]).to_string())
                    }
                }
                "top" => Ok("PID COMMAND\n1 java -jar -Dspring.profiles.active=api takserver.war\n".into()),
                "restart" => Ok(String::new()),
                other => Err(DockerError::Failed(format!("unexpected docker {other}"))),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::*;
    use super::*;

    #[tokio::test]
    async fn only_containers_of_the_project_are_seen() {
        let docker = FakeDocker { leaky_filter: true, ..Default::default() };
        let reports = container_reports(&docker, "tak-manager").await.unwrap();
        let names: Vec<&str> = reports.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["tak-manager-caddy-1", "tak-manager-takserver-1"], "the other project's container is dropped even if Docker lists it");
        assert_eq!(reports[0].repo_digests, vec!["caddy@sha256:1234".to_string()]);
        assert!(reports[1].repo_digests.is_empty(), "an image built here has no registry digest");
    }

    #[tokio::test]
    async fn an_image_gone_under_its_identifier_does_not_hide_the_others() {
        // Seen on the development machine: after an identical rebuild, a running container
        // still names an image identifier Docker no longer has.
        let docker = FakeDocker { image_gone_by_id: true, ..Default::default() };
        let reports = container_reports(&docker, "tak-manager").await.unwrap();
        let caddy = reports.iter().find(|c| c.service == "caddy").unwrap();
        assert_eq!(caddy.repo_digests, vec!["caddy@sha256:1234".to_string()], "found again by its name");
        let takserver = reports.iter().find(|c| c.service == "takserver").unwrap();
        assert_eq!(takserver.image_created.as_deref(), Some("2026-11-19T16:00:00Z"), "the image that is still there keeps its details");
        assert!(is_image_reference("vegardit/openldap@sha256:f6e7") && is_image_reference("localhost:5000/app:1"));
        assert!(!is_image_reference("--help") && !is_image_reference("caddy; id") && !is_image_reference(""));
    }

    #[tokio::test]
    async fn a_service_is_resolved_inside_the_project_only() {
        let docker = FakeDocker { leaky_filter: true, ..Default::default() };
        assert_eq!(service_container(&docker, "tak-manager", "caddy").await.unwrap().as_deref(), Some(CADDY_ID));
        assert_eq!(service_container(&docker, "tak-manager", "postgres").await.unwrap(), None);
        // The same service name exists in the other project: it is never the one returned.
        assert_ne!(service_container(&docker, "tak-manager", "caddy").await.unwrap().as_deref(), Some(FOREIGN_ID));
    }

    #[tokio::test]
    async fn a_docker_that_does_not_answer_is_an_error_not_an_empty_stack() {
        let docker = FakeDocker { down: true, ..Default::default() };
        assert!(matches!(container_reports(&docker, "tak-manager").await, Err(DockerError::Unavailable(_))));
    }

    #[test]
    fn container_identifiers() {
        assert!(is_container_id(TAK_ID) && is_container_id("0123456789ab"));
        for bad in ["", "abc", "tak-manager-caddy-1", "--help", "0123456789ab;rm"] {
            assert!(!is_container_id(bad), "{bad}");
        }
    }
}
