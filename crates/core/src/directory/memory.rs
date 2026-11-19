//! In-memory directory with the same rules as the LDAP one. Used by unit tests.

use std::sync::Mutex;

use async_trait::async_trait;

use super::{Directory, DirectoryError, DirectorySnapshot, DirectoryUser};

#[derive(Default)]
pub struct MemoryDirectory {
    state: Mutex<DirectorySnapshot>,
    passwords: Mutex<std::collections::BTreeMap<String, String>>,
    writes: Mutex<Vec<String>>,
    fail: Mutex<bool>,
}

impl MemoryDirectory {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the content, as if someone had edited the directory behind the platform's back.
    pub fn tamper(&self, edit: impl FnOnce(&mut DirectorySnapshot)) {
        edit(&mut self.state.lock().unwrap());
    }

    pub fn state(&self) -> DirectorySnapshot {
        self.state.lock().unwrap().clone()
    }

    pub fn password_of(&self, username: &str) -> Option<String> {
        self.passwords.lock().unwrap().get(username).cloned()
    }

    /// Every write received, in order (for example `add_member alpha alice`).
    pub fn writes(&self) -> Vec<String> {
        self.writes.lock().unwrap().clone()
    }

    pub fn clear_writes(&self) {
        self.writes.lock().unwrap().clear();
    }

    /// Makes every call fail, as when the directory is down.
    pub fn set_unavailable(&self, down: bool) {
        *self.fail.lock().unwrap() = down;
    }

    fn check(&self) -> Result<(), DirectoryError> {
        if *self.fail.lock().unwrap() {
            Err(DirectoryError::Unavailable("memory directory set unavailable".into()))
        } else {
            Ok(())
        }
    }

    fn log(&self, entry: String) {
        self.writes.lock().unwrap().push(entry);
    }
}

#[async_trait]
impl Directory for MemoryDirectory {
    async fn snapshot(&self) -> Result<DirectorySnapshot, DirectoryError> {
        self.check()?;
        Ok(self.state())
    }

    async fn create_user(&self, username: &str, password: &str, password_tag: &str) -> Result<(), DirectoryError> {
        self.check()?;
        self.log(format!("create_user {username}"));
        self.state.lock().unwrap().users.insert(username.into(), DirectoryUser { password_tag: Some(password_tag.into()) });
        self.passwords.lock().unwrap().insert(username.into(), password.into());
        Ok(())
    }

    async fn set_password(&self, username: &str, password: &str, password_tag: &str) -> Result<(), DirectoryError> {
        self.check()?;
        self.log(format!("set_password {username}"));
        let mut state = self.state.lock().unwrap();
        let user = state.users.get_mut(username).ok_or_else(|| DirectoryError::Rejected(format!("no such user {username}")))?;
        user.password_tag = Some(password_tag.into());
        self.passwords.lock().unwrap().insert(username.into(), password.into());
        Ok(())
    }

    async fn delete_user(&self, username: &str) -> Result<(), DirectoryError> {
        self.check()?;
        self.log(format!("delete_user {username}"));
        let mut state = self.state.lock().unwrap();
        state.users.remove(username);
        // Same effect as the referential-integrity overlay of OpenLDAP.
        for group in state.groups.values_mut() {
            group.members.remove(username);
        }
        state.groups.retain(|_, g| !g.members.is_empty() || !g.foreign_members.is_empty());
        self.passwords.lock().unwrap().remove(username);
        Ok(())
    }

    async fn add_member(&self, group: &str, username: &str) -> Result<(), DirectoryError> {
        self.check()?;
        self.log(format!("add_member {group} {username}"));
        let mut state = self.state.lock().unwrap();
        if !state.users.contains_key(username) {
            return Err(DirectoryError::Rejected(format!("no such user {username}")));
        }
        state.groups.entry(group.into()).or_default().members.insert(username.into());
        Ok(())
    }

    async fn remove_member(&self, group: &str, username: &str) -> Result<(), DirectoryError> {
        self.check()?;
        self.log(format!("remove_member {group} {username}"));
        let mut state = self.state.lock().unwrap();
        if let Some(g) = state.groups.get_mut(group) {
            g.members.remove(username);
            if g.members.is_empty() && g.foreign_members.is_empty() {
                state.groups.remove(group);
            }
        }
        Ok(())
    }

    async fn remove_foreign_member(&self, group: &str, member: &str) -> Result<(), DirectoryError> {
        self.check()?;
        self.log(format!("remove_foreign_member {group} {member}"));
        let mut state = self.state.lock().unwrap();
        if let Some(g) = state.groups.get_mut(group) {
            g.foreign_members.remove(member);
            if g.members.is_empty() && g.foreign_members.is_empty() {
                state.groups.remove(group);
            }
        }
        Ok(())
    }
}
