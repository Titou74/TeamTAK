//! What every request handler shares.

use std::collections::BTreeSet;
use std::sync::Arc;

use anyhow::{Context, Result};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use teamtak_core::directory::ldap::LdapDirectory;
use teamtak_core::directory::Directory;
use teamtak_core::package::build_truststore;
use teamtak_core::secret::SecretBox;
use teamtak_core::tak::http::{HttpTakConfig, HttpTakServer};
use teamtak_core::tak::TakServer;

use crate::config::Config;
use crate::mail::Mailer;
use crate::monitor::Monitor;
use crate::reconciler::Reconciler;

pub struct AppState {
    pub config: Config,
    pub db: PgPool,
    pub secrets: SecretBox,
    pub mailer: Arc<Mailer>,
    pub tak: Arc<dyn TakServer>,
    pub reconciler: Arc<Reconciler>,
    /// The monitoring loop and what it last saw.
    pub monitor: Arc<Monitor>,
    /// Truststore shipped in every zip: the CA certificates of TAK Server.
    pub truststore_p12: Vec<u8>,
}

pub type SharedState = Arc<AppState>;

pub async fn connect_database(config: &Config) -> Result<PgPool> {
    let db = PgPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(std::time::Duration::from_secs(10))
        .connect(&config.database_url)
        .await
        .context("connecting to PostgreSQL")?;
    sqlx::migrate!("./migrations").run(&db).await.context("applying database migrations")?;
    Ok(db)
}

/// DER certificates of a PEM file.
fn pem_certificates(pem: &[u8]) -> Result<Vec<Vec<u8>>> {
    use rustls_pki_types::pem::PemObject;
    use rustls_pki_types::CertificateDer;
    CertificateDer::pem_slice_iter(pem).map(|c| c.map(|c| c.to_vec()).map_err(|e| anyhow::anyhow!("{e}"))).collect()
}

/// The two backends the platform drives, and the CA certificates of TAK Server (DER).
pub struct Backends {
    pub directory: Arc<dyn Directory>,
    pub tak: Arc<dyn TakServer>,
    pub ca_ders: Vec<Vec<u8>>,
}

pub fn build_backends(config: &Config) -> Result<Backends> {
    let read = |path: &std::path::Path| std::fs::read(path).with_context(|| format!("reading {}", path.display()));
    let ca_pem = read(&config.tak.ca_file)?;
    let ca_ders = pem_certificates(&ca_pem).context("TAK_CA_FILE")?;
    let tak = HttpTakServer::new(HttpTakConfig {
        api_url: config.tak.api_url.clone(),
        enroll_url: config.tak.enroll_url.clone(),
        ca_pem,
        client_cert_pem: read(&config.tak.api_cert_file)?,
        client_key_pem: read(&config.tak.api_key_file)?,
        client_version: format!("teamtak-{}", env!("CARGO_PKG_VERSION")),
    })
    .context("setting up the TAK Server client")?;
    let directory = LdapDirectory::new(config.ldap.clone());
    Ok(Backends { directory: Arc::new(directory), tak: Arc::new(tak), ca_ders })
}

pub async fn build_state(config: Config) -> Result<SharedState> {
    let db = connect_database(&config).await?;
    let secrets = SecretBox::new(config.app_secret.as_bytes());
    let mailer = Arc::new(Mailer::new(&config.smtp, &config.public_url, Some(db.clone()))?);
    let Backends { directory, tak, ca_ders } = build_backends(&config)?;
    let truststore_p12 = build_truststore(&ca_ders, config.tak.p12_format).context("building the client truststore")?;
    let protected = BTreeSet::from([config.tak.api_cert_name.clone()]);
    let reconciler = Arc::new(Reconciler::new(db.clone(), directory.clone(), tak.clone(), secrets.clone(), protected));
    let monitor = Arc::new(Monitor::new(&config, db.clone(), tak.clone(), mailer.clone(), secrets.clone()));
    Ok(Arc::new(AppState { config, db, secrets, mailer, tak, reconciler, monitor, truststore_p12 }))
}
