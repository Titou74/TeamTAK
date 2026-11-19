//! The directory TAK Server reads accounts and groups from.
//!
//! [`Directory`] is the only way the rest of the code touches it. The production backend is
//! OpenLDAP ([`ldap::LdapDirectory`]); [`memory::MemoryDirectory`] backs the unit tests.

pub mod ldap;
pub mod memory;

use std::collections::{BTreeMap, BTreeSet};

use async_trait::async_trait;

#[derive(Debug, thiserror::Error)]
pub enum DirectoryError {
    #[error("directory unreachable: {0}")]
    Unavailable(String),
    #[error("directory refused the operation: {0}")]
    Rejected(String),
}

/// An account as it exists in the directory.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DirectoryUser {
    /// Fingerprint of the password last written by the platform (see `SecretBox::password_tag`).
    pub password_tag: Option<String>,
}

/// A group as it exists in the directory.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DirectoryGroup {
    /// Usernames of the members that are accounts of the directory.
    pub members: BTreeSet<String>,
    /// Members that are not accounts under the users branch, as raw identifiers (DNs).
    pub foreign_members: BTreeSet<String>,
}

/// Everything the platform manages in the directory, read in one go.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DirectorySnapshot {
    pub users: BTreeMap<String, DirectoryUser>,
    pub groups: BTreeMap<String, DirectoryGroup>,
}

impl DirectorySnapshot {
    /// Groups a user belongs to.
    pub fn groups_of(&self, username: &str) -> BTreeSet<String> {
        self.groups.iter().filter(|(_, g)| g.members.contains(username)).map(|(name, _)| name.clone()).collect()
    }
}

/// Write access to TAK accounts and groups.
///
/// A group exists only while it has members: `add_member` creates it when needed and
/// `remove_member` deletes it with its last member. Every method is idempotent, so a
/// reconciliation pass interrupted half-way can simply be run again.
#[async_trait]
pub trait Directory: Send + Sync {
    async fn snapshot(&self) -> Result<DirectorySnapshot, DirectoryError>;
    async fn create_user(&self, username: &str, password: &str, password_tag: &str) -> Result<(), DirectoryError>;
    async fn set_password(&self, username: &str, password: &str, password_tag: &str) -> Result<(), DirectoryError>;
    async fn delete_user(&self, username: &str) -> Result<(), DirectoryError>;
    async fn add_member(&self, group: &str, username: &str) -> Result<(), DirectoryError>;
    async fn remove_member(&self, group: &str, username: &str) -> Result<(), DirectoryError>;
    /// Removes a member that is not an account of the users branch, by its raw identifier.
    async fn remove_foreign_member(&self, group: &str, member: &str) -> Result<(), DirectoryError>;
}
