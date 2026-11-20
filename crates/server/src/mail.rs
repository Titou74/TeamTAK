//! Outgoing e-mail over SMTP (Brevo in production, a mail catcher locally).
//!
//! Every attempt is counted: a success moves "last e-mail sent", a failure is recorded
//! (which e-mail, why, never to whom). Most notifications are sent in the background, where
//! a failure is invisible to the user; the monitoring turns the count into an alert.

use anyhow::{Context, Result};
use lettre::message::header::ContentType;
use lettre::message::Mailbox;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use sqlx::PgPool;

use crate::config::{SmtpConfig, SmtpTls};
use crate::locale::{self, Rendered, Template};

pub struct Mailer {
    transport: Option<AsyncSmtpTransport<Tokio1Executor>>,
    from: Mailbox,
    public_url: String,
    /// Where attempts are counted; `None` in the unit tests.
    db: Option<PgPool>,
}

/// Longest failure text kept.
const MAX_ERROR: usize = 300;

/// A failure text fit to be stored and shown: one line, bounded, and without the e-mail
/// addresses a mail server likes to repeat in its answers.
pub fn scrub_error(error: &str) -> String {
    let single_line: String = error.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let words: Vec<String> = single_line
        .split_whitespace()
        .map(|word| if word.contains('@') { "<address>".to_string() } else { word.to_string() })
        .collect();
    words.join(" ").chars().take(MAX_ERROR).collect()
}

impl Mailer {
    pub fn new(config: &SmtpConfig, public_url: &str, db: Option<PgPool>) -> Result<Self> {
        let from: Mailbox = config.from.parse().with_context(|| format!("MAIL_FROM={:?} is not an address", config.from))?;
        let transport = if config.host.is_empty() {
            tracing::warn!("SMTP_HOST is empty: no e-mail will be sent");
            None
        } else {
            let builder = match config.tls {
                SmtpTls::StartTls => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&config.host)?,
                SmtpTls::Tls => AsyncSmtpTransport::<Tokio1Executor>::relay(&config.host)?,
                SmtpTls::None => AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&config.host),
            };
            let builder = builder.port(config.port).timeout(Some(std::time::Duration::from_secs(15)));
            let builder = if config.username.is_empty() {
                builder
            } else {
                builder.credentials(Credentials::new(config.username.clone(), config.password.clone()))
            };
            Some(builder.build())
        };
        Ok(Self { transport, from, public_url: public_url.to_string(), db })
    }

    /// False when no mail server is set: nothing can be sent.
    pub fn is_configured(&self) -> bool {
        self.transport.is_some()
    }

    /// Sends the test e-mail to `to`, written in `language`. Counted like any other: a
    /// success shows on the monitoring page as the last e-mail sent, a failure as the last
    /// one that failed.
    pub async fn send_test(&self, to: &str, language: &str) -> Result<()> {
        let date = locale::format_datetime(language, time::OffsetDateTime::now_utc());
        self.send(to, language, Template::MailTest, &[("date", &date), ("url", &self.public_url)]).await
    }

    async fn deliver(&self, to: &str, rendered: Rendered) -> Result<()> {
        let transport = self.transport.as_ref().context("no SMTP server configured")?;
        let message = Message::builder()
            .from(self.from.clone())
            .to(to.parse().with_context(|| format!("recipient {to:?}"))?)
            .subject(rendered.subject)
            .header(ContentType::TEXT_PLAIN)
            .body(rendered.body)?;
        transport.send(message).await.context("SMTP delivery")?;
        Ok(())
    }

    /// Counts the attempt. Never fails: the count must not break what it counts.
    async fn count(&self, template: Template, outcome: &Result<()>) {
        let Some(db) = &self.db else { return };
        let written = match outcome {
            Ok(()) => sqlx::query("UPDATE monitor_state SET last_mail_ok_at = now() WHERE id = 1").execute(db).await,
            Err(e) => {
                let recorded = sqlx::query("INSERT INTO mail_failures (template, error) VALUES ($1, $2)")
                    .bind(template.key())
                    .bind(scrub_error(&format!("{e:#}")))
                    .execute(db)
                    .await;
                match recorded {
                    Ok(_) => sqlx::query("UPDATE monitor_state SET last_mail_failure_at = now() WHERE id = 1").execute(db).await,
                    Err(e) => Err(e),
                }
            }
        };
        if let Err(e) = written {
            tracing::warn!(error = %e, "e-mail attempt not counted");
        }
    }

    /// Sends an e-mail that was rendered already.
    pub async fn send_rendered(&self, to: &str, template: Template, rendered: Rendered) -> Result<()> {
        let outcome = self.deliver(to, rendered).await;
        self.count(template, &outcome).await;
        outcome
    }

    /// Sends `template` to `to`, written in `language` (English when unknown).
    pub async fn send(&self, to: &str, language: &str, template: Template, vars: &[(&str, &str)]) -> Result<()> {
        self.send_rendered(to, template, locale::render(template, language, vars, &self.public_url)).await
    }

    /// Like [`send`], for notifications whose failure must not fail the request: logged, and
    /// counted for the monitoring.
    pub async fn send_best_effort(&self, to: &str, language: &str, template: Template, vars: &[(&str, &str)]) {
        if let Err(e) = self.send(to, language, template, vars).await {
            tracing::warn!(to, template = template.key(), error = %format!("{e:#}"), "e-mail not sent");
        }
    }

    pub async fn send_rendered_best_effort(&self, to: &str, template: Template, rendered: Rendered) {
        if let Err(e) = self.send_rendered(to, template, rendered).await {
            tracing::warn!(to, template = template.key(), error = %format!("{e:#}"), "e-mail not sent");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_texts_keep_no_address_and_stay_short() {
        let raw = "SMTP delivery: permanent error (550): 5.1.1 <jane.doe@example.org>: Recipient address rejected:\r\nUser unknown in relay=alice@example.org";
        assert_eq!(
            scrub_error(raw),
            "SMTP delivery: permanent error (550): 5.1.1 <address> Recipient address rejected: User unknown in <address>"
        );
        assert_eq!(scrub_error(&"x".repeat(1000)).chars().count(), MAX_ERROR);
        assert_eq!(scrub_error("no SMTP server configured"), "no SMTP server configured");
    }

    #[tokio::test]
    async fn without_a_mail_server_sending_fails_cleanly() {
        let config = SmtpConfig { host: String::new(), port: 587, username: String::new(), password: String::new(), tls: SmtpTls::StartTls, from: "TeamTAK <tak@localhost>".into() };
        let mailer = Mailer::new(&config, "https://tak.example.org", None).unwrap();
        assert!(!mailer.is_configured());
        let error = mailer.send("someone@example.org", "en", Template::LoginCode, &[("code", "123456"), ("minutes", "10")]).await.unwrap_err();
        assert!(format!("{error:#}").contains("no SMTP server configured"));
        let error = mailer.send_test("someone@example.org", "fr").await.unwrap_err();
        assert!(format!("{error:#}").contains("no SMTP server configured"));
    }
}
