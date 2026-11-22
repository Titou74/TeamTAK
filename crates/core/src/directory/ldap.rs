//! OpenLDAP backend, with the layout validated in phase 0:
//!
//! - accounts: `uid=<username>,ou=People,<base>` (`inetOrgPerson`);
//! - groups: `cn=<group>,ou=Group,<base>` (`groupOfNames`, attribute `member`); the memberOf
//!   overlay turns them into the `memberOf` attribute TAK Server reads;
//! - the platform binds as `cn=teamtak,ou=Services,<base>`, the only writer.
//!
//! Passwords are sent in clear in the `userPassword` attribute and hashed by OpenLDAP itself
//! (Argon2, `olcPPolicyHashCleartext`). The connection is plain `ldap://` inside the Docker
//! network for now; see the phase 1 report.

use std::collections::HashSet;
use std::time::Duration;

use async_trait::async_trait;
use ldap3::{dn_escape, Ldap, LdapConnAsync, LdapConnSettings, LdapError, Mod, Scope, SearchEntry};
use tokio::sync::Mutex;

use super::{Directory, DirectoryError, DirectoryGroup, DirectorySnapshot, DirectoryUser};

const RC_SIZE_LIMIT: u32 = 4;
const RC_NO_SUCH_ATTRIBUTE: u32 = 16;
const RC_VALUE_EXISTS: u32 = 20;
const RC_NO_SUCH_OBJECT: u32 = 32;
const RC_OBJECT_CLASS_VIOLATION: u32 = 65;
const RC_ALREADY_EXISTS: u32 = 68;

/// Prefix of the `description` value that carries the password tag of an account.
const TAG_PREFIX: &str = "teamtak-pwtag=";

#[derive(Debug, Clone)]
pub struct LdapConfig {
    /// For example `ldap://openldap:389`.
    pub url: String,
    /// For example `dc=tak,dc=example`.
    pub base_dn: String,
    pub bind_dn: String,
    pub bind_password: String,
}

pub struct LdapDirectory {
    config: LdapConfig,
    conn: Mutex<Option<Ldap>>,
}

fn unavailable(e: impl std::fmt::Display) -> DirectoryError {
    DirectoryError::Unavailable(e.to_string())
}

/// Result code of a finished operation, or the transport error.
fn rc_of(result: Result<ldap3::LdapResult, LdapError>) -> Result<(u32, String), DirectoryError> {
    match result {
        Ok(r) => Ok((r.rc, r.text)),
        Err(e) => Err(unavailable(e)),
    }
}

fn rejected(op: &str, rc: u32, text: &str) -> DirectoryError {
    DirectoryError::Rejected(format!("{op}: LDAP result {rc} {text}"))
}

impl LdapDirectory {
    pub fn new(config: LdapConfig) -> Self {
        Self { config, conn: Mutex::new(None) }
    }

    pub fn user_dn(&self, username: &str) -> String {
        format!("uid={},ou=People,{}", dn_escape(username), self.config.base_dn)
    }

    pub fn group_dn(&self, group: &str) -> String {
        format!("cn={},ou=Group,{}", dn_escape(group), self.config.base_dn)
    }

    /// A bound connection, opened on first use and reopened after a failure.
    async fn conn(&self) -> Result<Ldap, DirectoryError> {
        let mut slot = self.conn.lock().await;
        if let Some(ldap) = slot.as_mut() {
            if !ldap.is_closed() {
                return Ok(ldap.clone());
            }
        }
        let settings = LdapConnSettings::new().set_conn_timeout(Duration::from_secs(5));
        let (conn, mut ldap) = LdapConnAsync::with_settings(settings, &self.config.url).await.map_err(unavailable)?;
        ldap3::drive!(conn);
        ldap.with_timeout(Duration::from_secs(15));
        let (rc, text) = rc_of(ldap.simple_bind(&self.config.bind_dn, &self.config.bind_password).await)?;
        if rc != 0 {
            return Err(DirectoryError::Unavailable(format!("bind as {} refused: {rc} {text}", self.config.bind_dn)));
        }
        *slot = Some(ldap.clone());
        Ok(ldap)
    }

    async fn drop_conn(&self) {
        *self.conn.lock().await = None;
    }

    /// Runs one operation; a transport error drops the cached connection so the next call
    /// reconnects.
    async fn run<F, Fut>(&self, op: F) -> Result<(u32, String), DirectoryError>
    where
        F: FnOnce(Ldap) -> Fut,
        Fut: std::future::Future<Output = Result<ldap3::LdapResult, LdapError>>,
    {
        let mut ldap = self.conn().await?;
        ldap.with_timeout(Duration::from_secs(15));
        match rc_of(op(ldap).await) {
            Err(e) => {
                self.drop_conn().await;
                Err(e)
            }
            ok => ok,
        }
    }

    async fn search(&self, base: &str, filter: &str, attrs: Vec<&str>) -> Result<Vec<SearchEntry>, DirectoryError> {
        let mut ldap = self.conn().await?;
        ldap.with_timeout(Duration::from_secs(30));
        let result = match ldap.search(base, Scope::OneLevel, filter, attrs).await {
            Ok(r) => r,
            Err(e) => {
                self.drop_conn().await;
                return Err(unavailable(e));
            }
        };
        let ldap3::SearchResult(entries, res) = result;
        match res.rc {
            0 => Ok(entries.into_iter().map(SearchEntry::construct).collect()),
            // install.sh lifts the limit for the platform's account (deploy/ldap/service_limits.ldif).
            RC_SIZE_LIMIT => Err(DirectoryError::Rejected(format!(
                "search under {base} hit the directory's size limit; run ./install.sh again, which lifts it for the platform"
            ))),
            rc => Err(rejected("search", rc, &res.text)),
        }
    }

    /// Username carried by a member DN when it is an account of the users branch.
    fn member_username(&self, dn: &str) -> Option<String> {
        let suffix = format!(",ou=people,{}", self.config.base_dn.to_lowercase());
        let lower = dn.to_lowercase();
        let rdn = lower.strip_suffix(&suffix)?.strip_prefix("uid=")?;
        let plain = !rdn.is_empty() && rdn.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b));
        // The uid is matched case-insensitively by LDAP; platform usernames are lowercase.
        plain.then(|| rdn.to_string())
    }
}

fn set<'a>(values: &[&'a str]) -> HashSet<&'a str> {
    values.iter().copied().collect()
}

#[async_trait]
impl Directory for LdapDirectory {
    async fn snapshot(&self) -> Result<DirectorySnapshot, DirectoryError> {
        let mut snapshot = DirectorySnapshot::default();
        let people = format!("ou=People,{}", self.config.base_dn);
        for entry in self.search(&people, "(objectClass=inetOrgPerson)", vec!["uid", "description"]).await? {
            let Some(uid) = entry.attrs.get("uid").and_then(|v| v.first()) else { continue };
            let password_tag =
                entry.attrs.get("description").into_iter().flatten().find_map(|d| d.strip_prefix(TAG_PREFIX).map(str::to_string));
            snapshot.users.insert(uid.to_lowercase(), DirectoryUser { password_tag });
        }
        let groups = format!("ou=Group,{}", self.config.base_dn);
        for entry in self.search(&groups, "(objectClass=groupOfNames)", vec!["cn", "member"]).await? {
            let Some(cn) = entry.attrs.get("cn").and_then(|v| v.first()) else { continue };
            let mut group = DirectoryGroup::default();
            for member in entry.attrs.get("member").into_iter().flatten() {
                match self.member_username(member) {
                    Some(username) => group.members.insert(username),
                    None => group.foreign_members.insert(member.clone()),
                };
            }
            snapshot.groups.insert(cn.clone(), group);
        }
        Ok(snapshot)
    }

    async fn create_user(&self, username: &str, password: &str, password_tag: &str) -> Result<(), DirectoryError> {
        let dn = self.user_dn(username);
        let tag = format!("{TAG_PREFIX}{password_tag}");
        let attrs = vec![
            ("objectClass", set(&["top", "person", "organizationalPerson", "inetOrgPerson"])),
            ("uid", set(&[username])),
            ("cn", set(&[username])),
            ("sn", set(&[username])),
            ("description", set(&[tag.as_str()])),
            ("userPassword", set(&[password])),
        ];
        let (rc, text) = self.run(|mut l| async move { l.add(&dn, attrs).await }).await?;
        match rc {
            0 => Ok(()),
            // Left over from an interrupted pass: make sure it carries the expected password.
            RC_ALREADY_EXISTS => self.set_password(username, password, password_tag).await,
            rc => Err(rejected("add user", rc, &text)),
        }
    }

    async fn set_password(&self, username: &str, password: &str, password_tag: &str) -> Result<(), DirectoryError> {
        let dn = self.user_dn(username);
        let tag = format!("{TAG_PREFIX}{password_tag}");
        let mods = vec![Mod::Replace("userPassword", set(&[password])), Mod::Replace("description", set(&[tag.as_str()]))];
        let (rc, text) = self.run(|mut l| async move { l.modify(&dn, mods).await }).await?;
        match rc {
            0 => Ok(()),
            rc => Err(rejected("set password", rc, &text)),
        }
    }

    async fn delete_user(&self, username: &str) -> Result<(), DirectoryError> {
        let dn = self.user_dn(username);
        let (rc, text) = self.run(|mut l| async move { l.delete(&dn).await }).await?;
        match rc {
            0 | RC_NO_SUCH_OBJECT => Ok(()),
            rc => Err(rejected("delete user", rc, &text)),
        }
    }

    async fn add_member(&self, group: &str, username: &str) -> Result<(), DirectoryError> {
        let dn = self.group_dn(group);
        let member = self.user_dn(username);
        let mods = vec![Mod::Add("member", set(&[member.as_str()]))];
        let (rc, text) = {
            let dn = dn.clone();
            self.run(|mut l| async move { l.modify(&dn, mods).await }).await?
        };
        match rc {
            0 | RC_VALUE_EXISTS => Ok(()),
            RC_NO_SUCH_OBJECT => {
                // A groupOfNames cannot be empty: the group is created with its first member.
                let attrs = vec![
                    ("objectClass", set(&["top", "groupOfNames"])),
                    ("cn", set(&[group])),
                    ("member", set(&[member.as_str()])),
                ];
                let (rc, text) = self.run(|mut l| async move { l.add(&dn, attrs).await }).await?;
                match rc {
                    0 => Ok(()),
                    rc => Err(rejected("add group", rc, &text)),
                }
            }
            rc => Err(rejected("add member", rc, &text)),
        }
    }

    async fn remove_member(&self, group: &str, username: &str) -> Result<(), DirectoryError> {
        let member = self.user_dn(username);
        self.remove_foreign_member(group, &member).await
    }

    async fn remove_foreign_member(&self, group: &str, member: &str) -> Result<(), DirectoryError> {
        let dn = self.group_dn(group);
        let mods = vec![Mod::Delete("member", set(&[member]))];
        let (rc, text) = {
            let dn = dn.clone();
            self.run(|mut l| async move { l.modify(&dn, mods).await }).await?
        };
        match rc {
            0 | RC_NO_SUCH_ATTRIBUTE | RC_NO_SUCH_OBJECT => Ok(()),
            RC_OBJECT_CLASS_VIOLATION => {
                // It was the last member: the group goes with it.
                let (rc, text) = self.run(|mut l| async move { l.delete(&dn).await }).await?;
                match rc {
                    0 | RC_NO_SUCH_OBJECT => Ok(()),
                    rc => Err(rejected("delete group", rc, &text)),
                }
            }
            rc => Err(rejected("remove member", rc, &text)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> LdapDirectory {
        LdapDirectory::new(LdapConfig {
            url: "ldap://unused".into(),
            base_dn: "dc=tak,dc=example".into(),
            bind_dn: "cn=teamtak,ou=Services,dc=tak,dc=example".into(),
            bind_password: "x".into(),
        })
    }

    #[test]
    fn dns_follow_the_phase_0_layout() {
        let d = dir();
        assert_eq!(d.user_dn("alpha-shared"), "uid=alpha-shared,ou=People,dc=tak,dc=example");
        assert_eq!(d.group_dn("alpha"), "cn=alpha,ou=Group,dc=tak,dc=example");
    }

    #[test]
    fn special_characters_cannot_escape_the_branch() {
        let d = dir();
        let dn = d.user_dn("a,ou=Services");
        assert_eq!(dn, "uid=a\\2cou\\3dServices,ou=People,dc=tak,dc=example");
    }

    #[test]
    fn member_dns_are_mapped_to_usernames() {
        let d = dir();
        assert_eq!(d.member_username("uid=alice,ou=People,dc=tak,dc=example"), Some("alice".into()));
        assert_eq!(d.member_username("UID=Alice,OU=people,DC=tak,DC=example"), Some("alice".into()));
        assert_eq!(d.member_username("cn=takserver,ou=Services,dc=tak,dc=example"), None);
        assert_eq!(d.member_username("uid=a\\,b,ou=People,dc=tak,dc=example"), None);
        assert_eq!(d.member_username("uid=alice,ou=People,dc=other"), None);
    }
}
