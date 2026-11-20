//! Reconciliation: make the directory and TAK Server match what the platform's database says.
//!
//! The database is the source of truth. A pass reads the actual state, computes the
//! difference with pure functions ([`plan_directory`], [`plan_revocations`],
//! [`plan_sessions`]) and applies it through the two backend traits. Every step is
//! idempotent: a pass that finds nothing to do changes nothing, and a pass interrupted
//! half-way is completed by the next one.
//!
//! Rules taken from phase 0:
//! - withdrawing an access means revoking its certificates first, then deleting the account;
//! - after a membership change the server is told to re-read the user's groups at once;
//! - a session whose account is left with no group keeps its old groups until it is cut;
//!
//! and one found in phase 1: a session opened while its account had no group never receives
//! the groups added later, so it is cut as well (the client reconnects by itself).

use std::collections::{BTreeMap, BTreeSet};

use crate::directory::{Directory, DirectorySnapshot};
use crate::tak::{IssuedCertificate, Subscription, TakServer};

/// A TAK account that must exist, with the groups it must be in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesiredAccount {
    pub username: String,
    pub password: String,
    /// Fingerprint of `password`, compared with the one stored in the directory.
    pub password_tag: String,
    /// Empty when the account's team is suspended: the account stays, without any group.
    pub groups: BTreeSet<String>,
}

/// What the database wants.
#[derive(Debug, Clone, Default)]
pub struct DesiredState {
    pub accounts: Vec<DesiredAccount>,
    /// Fingerprints of certificates whose revocation was requested (regenerated accesses).
    pub revoke_fingerprints: BTreeSet<String>,
    /// Usernames the platform never touches in TAK Server, such as its own API certificate.
    pub protected_usernames: BTreeSet<String>,
}

impl DesiredState {
    fn by_username(&self) -> BTreeMap<&str, &DesiredAccount> {
        self.accounts.iter().map(|a| (a.username.as_str(), a)).collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectoryAction {
    CreateUser { username: String },
    SetPassword { username: String },
    AddMember { group: String, username: String },
    RemoveMember { group: String, username: String },
    RemoveForeignMember { group: String, member: String },
    DeleteUser { username: String },
}

/// Difference between the desired accounts and the directory, in the order it must be
/// applied: accounts are created before they join a group, and leave their groups before
/// they are deleted.
pub fn plan_directory(desired: &DesiredState, actual: &DirectorySnapshot) -> Vec<DirectoryAction> {
    let wanted = desired.by_username();
    let mut creations = Vec::new();
    let mut passwords = Vec::new();
    for account in &desired.accounts {
        match actual.users.get(&account.username) {
            None => creations.push(DirectoryAction::CreateUser { username: account.username.clone() }),
            Some(user) if user.password_tag.as_deref() != Some(account.password_tag.as_str()) => {
                passwords.push(DirectoryAction::SetPassword { username: account.username.clone() })
            }
            Some(_) => {}
        }
    }

    let mut additions = Vec::new();
    for account in &desired.accounts {
        for group in &account.groups {
            let present = actual.groups.get(group).is_some_and(|g| g.members.contains(&account.username));
            if !present {
                additions.push(DirectoryAction::AddMember { group: group.clone(), username: account.username.clone() });
            }
        }
    }

    let mut removals = Vec::new();
    for (name, group) in &actual.groups {
        for member in &group.members {
            let expected = wanted.get(member.as_str()).is_some_and(|a| a.groups.contains(name));
            if !expected {
                removals.push(DirectoryAction::RemoveMember { group: name.clone(), username: member.clone() });
            }
        }
        for member in &group.foreign_members {
            removals.push(DirectoryAction::RemoveForeignMember { group: name.clone(), member: member.clone() });
        }
    }

    let deletions = actual
        .users
        .keys()
        .filter(|u| !wanted.contains_key(u.as_str()))
        .map(|u| DirectoryAction::DeleteUser { username: u.clone() });

    creations.into_iter().chain(passwords).chain(additions).chain(removals).chain(deletions).collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevocationReason {
    /// The platform asked for it (regenerated access).
    Requested,
    /// The certificate belongs to no access of the platform (removed access, or unknown).
    NoAccess,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Revocation {
    pub certificate_id: i64,
    pub username: String,
    pub fingerprint: String,
    pub reason: RevocationReason,
}

/// Certificates to revoke among the active ones.
pub fn plan_revocations(desired: &DesiredState, active: &[IssuedCertificate]) -> Vec<Revocation> {
    let wanted = desired.by_username();
    active
        .iter()
        .filter(|c| !desired.protected_usernames.contains(&c.username))
        .filter_map(|c| {
            let reason = if !wanted.contains_key(c.username.as_str()) {
                RevocationReason::NoAccess
            } else if desired.revoke_fingerprints.contains(&c.fingerprint) {
                RevocationReason::Requested
            } else {
                return None;
            };
            Some(Revocation { certificate_id: c.id, username: c.username.clone(), fingerprint: c.fingerprint.clone(), reason })
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionAction {
    /// The session cannot be brought in line while it stays open: it holds groups although
    /// its account has none left (or no longer exists), or it holds none although its
    /// account now has some. The client reconnects by itself and gets the right groups.
    Cut {
        subscription_uid: String,
        /// The account, or the device's callsign when TAK Server gives no username.
        username: String,
        /// A group-less session TAK Server lists without a username. The list also keeps
        /// entries of connections that are already gone and answers 400 when asked to cut
        /// them, so a failure to cut one of these is not an error.
        best_effort: bool,
    },
    /// The session's groups differ from the directory's: ask the server to re-read them.
    Refresh { username: String },
}

/// What to do about open sessions whose groups are not the expected ones.
///
/// TAK Server lists a session that has no group without its username, so such a session
/// cannot be matched to an account. It sees nobody and nobody sees it; when
/// `cut_groupless` is set (the pass has just changed accounts, memberships or certificates)
/// these sessions are cut, so that a device whose account has just been given a group
/// reconnects and gets it, and a device whose access has just been withdrawn does not linger.
pub fn plan_sessions(desired: &DesiredState, subscriptions: &[Subscription], cut_groupless: bool) -> Vec<SessionAction> {
    let wanted = desired.by_username();
    let mut actions = Vec::new();
    let mut refreshed = BTreeSet::new();
    for sub in subscriptions {
        if sub.username.is_empty() {
            if cut_groupless && sub.groups.is_empty() {
                let who = sub.callsign.clone().or_else(|| sub.client_uid.clone()).unwrap_or_default();
                actions.push(SessionAction::Cut {
                    subscription_uid: sub.subscription_uid.clone(),
                    username: who,
                    best_effort: true,
                });
            }
            continue;
        }
        if desired.protected_usernames.contains(&sub.username) {
            continue;
        }
        let expected: BTreeSet<&str> =
            wanted.get(sub.username.as_str()).map(|a| a.groups.iter().map(String::as_str).collect()).unwrap_or_default();
        let held: BTreeSet<&str> = sub.groups.iter().map(String::as_str).collect();
        if held == expected {
            continue;
        }
        if expected.is_empty() || held.is_empty() {
            // TAK Server only updates the groups of an open session that has at least one
            // group before and after the change: it neither takes the last group away
            // (phase 0) nor gives a first group to a session opened without any (phase 1).
            actions.push(SessionAction::Cut {
                subscription_uid: sub.subscription_uid.clone(),
                username: sub.username.clone(),
                best_effort: false,
            });
        } else if refreshed.insert(sub.username.clone()) {
            actions.push(SessionAction::Refresh { username: sub.username.clone() });
        }
    }
    actions
}

/// Something a pass changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    UserCreated { username: String },
    PasswordSet { username: String },
    MemberAdded { group: String, username: String },
    MemberRemoved { group: String, username: String },
    ForeignMemberRemoved { group: String, member: String },
    UserDeleted { username: String },
    CertificateRevoked { username: String, fingerprint: String, reason: RevocationReason },
    SessionCut { username: String, subscription_uid: String },
    GroupsRefreshed { username: String },
}

/// Every value [`Change::kind`] can return; the UI has a text for each
/// (`reconcile.kinds.<kind>`).
pub const CHANGE_KINDS: &[&str] = &[
    "user_created",
    "password_set",
    "member_added",
    "member_removed",
    "foreign_member_removed",
    "user_deleted",
    "certificate_revoked",
    "orphan_certificate_revoked",
    "session_cut",
    "groups_refreshed",
];

impl Change {
    /// Stable identifier, translated by the UI.
    pub fn kind(&self) -> &'static str {
        match self {
            Change::UserCreated { .. } => "user_created",
            Change::PasswordSet { .. } => "password_set",
            Change::MemberAdded { .. } => "member_added",
            Change::MemberRemoved { .. } => "member_removed",
            Change::ForeignMemberRemoved { .. } => "foreign_member_removed",
            Change::UserDeleted { .. } => "user_deleted",
            Change::CertificateRevoked { reason: RevocationReason::Requested, .. } => "certificate_revoked",
            Change::CertificateRevoked { reason: RevocationReason::NoAccess, .. } => "orphan_certificate_revoked",
            Change::SessionCut { .. } => "session_cut",
            Change::GroupsRefreshed { .. } => "groups_refreshed",
        }
    }

    /// The account concerned (the raw member identifier for a foreign member; the callsign
    /// of the device for a session TAK Server lists without a username).
    pub fn subject(&self) -> &str {
        match self {
            Change::UserCreated { username }
            | Change::PasswordSet { username }
            | Change::MemberAdded { username, .. }
            | Change::MemberRemoved { username, .. }
            | Change::UserDeleted { username }
            | Change::CertificateRevoked { username, .. }
            | Change::SessionCut { username, .. }
            | Change::GroupsRefreshed { username } => username,
            Change::ForeignMemberRemoved { member, .. } => member,
        }
    }

    /// The group, when the change is about a membership.
    pub fn group(&self) -> Option<&str> {
        match self {
            Change::MemberAdded { group, .. }
            | Change::MemberRemoved { group, .. }
            | Change::ForeignMemberRemoved { group, .. } => Some(group),
            _ => None,
        }
    }
}

/// Outcome of one pass.
#[derive(Debug, Clone, Default)]
pub struct PassReport {
    pub changes: Vec<Change>,
    /// Steps that failed, in plain English for the logs and the administrator.
    pub errors: Vec<String>,
    pub directory_reachable: bool,
    pub tak_reachable: bool,
    /// Certificates still active at the end of the pass, when TAK Server could be read.
    pub active_certificates: Option<Vec<IssuedCertificate>>,
    /// Fingerprints revoked by this pass.
    pub revoked: Vec<String>,
}

impl PassReport {
    pub fn is_clean(&self) -> bool {
        self.changes.is_empty() && self.errors.is_empty()
    }
}

/// Safety settings of a pass.
#[derive(Debug, Clone, Copy, Default)]
pub struct PassOptions {
    /// When set, a pass that finds more accounts than this in the directory that the database
    /// does not know changes nothing and reports an error instead. It protects against an
    /// empty or wrong database (a bad restore, for example) wiping every TAK account; passes
    /// that follow a change made on the platform, or asked for by an administrator, set no
    /// limit.
    pub max_unknown_accounts: Option<usize>,
}

/// Runs one reconciliation pass with no safety limit.
pub async fn run_pass(desired: &DesiredState, directory: &dyn Directory, tak: &dyn TakServer) -> PassReport {
    run_pass_with(desired, directory, tak, PassOptions::default()).await
}

/// Runs one reconciliation pass.
pub async fn run_pass_with(
    desired: &DesiredState,
    directory: &dyn Directory,
    tak: &dyn TakServer,
    options: PassOptions,
) -> PassReport {
    let mut report = PassReport::default();
    let passwords: BTreeMap<&str, &DesiredAccount> = desired.by_username();

    // 1. Directory: everything except account deletions.
    let mut deletions: Vec<String> = Vec::new();
    let mut touched: BTreeSet<String> = BTreeSet::new();
    // Set when an account that was in no group is given one: its open sessions, which TAK
    // Server lists without a username, will never receive it.
    let mut gave_a_first_group = false;
    match directory.snapshot().await {
        Err(e) => report.errors.push(format!("reading the directory: {e}")),
        Ok(snapshot) => {
            report.directory_reachable = true;
            let unknown = snapshot.users.keys().filter(|u| !passwords.contains_key(u.as_str())).count();
            if options.max_unknown_accounts.is_some_and(|max| unknown > max) {
                report.errors.push(format!(
                    "the directory holds {unknown} accounts the database does not know; nothing was changed. \
                     If the database is right, run a reconciliation from the administration area to delete them."
                ));
                return report;
            }
            let had_a_group: BTreeSet<&str> =
                snapshot.groups.values().flat_map(|g| g.members.iter().map(String::as_str)).collect();
            for action in plan_directory(desired, &snapshot) {
                let (result, change) = match &action {
                    DirectoryAction::CreateUser { username } => {
                        let a = passwords[username.as_str()];
                        (
                            directory.create_user(username, &a.password, &a.password_tag).await,
                            Change::UserCreated { username: username.clone() },
                        )
                    }
                    DirectoryAction::SetPassword { username } => {
                        let a = passwords[username.as_str()];
                        (
                            directory.set_password(username, &a.password, &a.password_tag).await,
                            Change::PasswordSet { username: username.clone() },
                        )
                    }
                    DirectoryAction::AddMember { group, username } => (
                        directory.add_member(group, username).await,
                        Change::MemberAdded { group: group.clone(), username: username.clone() },
                    ),
                    DirectoryAction::RemoveMember { group, username } => (
                        directory.remove_member(group, username).await,
                        Change::MemberRemoved { group: group.clone(), username: username.clone() },
                    ),
                    DirectoryAction::RemoveForeignMember { group, member } => (
                        directory.remove_foreign_member(group, member).await,
                        Change::ForeignMemberRemoved { group: group.clone(), member: member.clone() },
                    ),
                    DirectoryAction::DeleteUser { username } => {
                        deletions.push(username.clone());
                        continue;
                    }
                };
                match result {
                    Ok(()) => {
                        if matches!(change, Change::MemberAdded { .. } | Change::MemberRemoved { .. }) {
                            touched.insert(change.subject().to_string());
                        }
                        if matches!(change, Change::MemberAdded { .. }) && !had_a_group.contains(change.subject()) {
                            gave_a_first_group = true;
                        }
                        report.changes.push(change);
                    }
                    Err(e) => report.errors.push(format!("{action:?}: {e}")),
                }
            }
        }
    }

    // 2. Certificates: revoke what must not work any more.
    let mut still_active: BTreeSet<String> = BTreeSet::new();
    match tak.active_certificates().await {
        Err(e) => report.errors.push(format!("listing certificates: {e}")),
        Ok(active) => {
            report.tak_reachable = true;
            let mut remaining = active.clone();
            for revocation in plan_revocations(desired, &active) {
                match tak.revoke_certificate(revocation.certificate_id).await {
                    Ok(()) => {
                        remaining.retain(|c| c.id != revocation.certificate_id);
                        report.revoked.push(revocation.fingerprint.clone());
                        report.changes.push(Change::CertificateRevoked {
                            username: revocation.username,
                            fingerprint: revocation.fingerprint,
                            reason: revocation.reason,
                        });
                    }
                    Err(e) => report.errors.push(format!("revoking certificate {}: {e}", revocation.certificate_id)),
                }
            }
            still_active = remaining.iter().map(|c| c.username.clone()).collect();
            report.active_certificates = Some(remaining);
        }
    }

    // 3. Account deletions, only once the account has no working certificate left.
    for username in deletions {
        if !report.tak_reachable {
            report.errors.push(format!("deleting {username} postponed: its certificates could not be checked"));
        } else if still_active.contains(&username) {
            report.errors.push(format!("deleting {username} postponed: a certificate is still active"));
        } else {
            match directory.delete_user(&username).await {
                Ok(()) => report.changes.push(Change::UserDeleted { username }),
                Err(e) => report.errors.push(format!("deleting {username}: {e}")),
            }
        }
    }

    if !report.tak_reachable {
        return report;
    }

    // 4. Push membership changes to connected clients.
    for username in &touched {
        if let Err(e) = tak.refresh_groups(username).await {
            report.errors.push(format!("refreshing the groups of {username}: {e}"));
        }
    }

    // 5. Sessions that still hold groups they should not, or miss the ones they should have.
    // Group-less sessions are cut when one of them may be waiting for a group it will never
    // receive, or may belong to an access that was just withdrawn. A pass that only moved
    // accounts in and out of additional groups (an event starting or ending) leaves them
    // alone: every account concerned already had a group, so none of them is group-less.
    let only_extra_groups_moved = !gave_a_first_group
        && report.changes.iter().all(|c| matches!(c, Change::MemberAdded { .. } | Change::MemberRemoved { .. }));
    let changed_something = !report.changes.is_empty() && !only_extra_groups_moved;
    match tak.subscriptions().await {
        Err(e) => report.errors.push(format!("listing sessions: {e}")),
        Ok(subscriptions) => {
            for action in plan_sessions(desired, &subscriptions, changed_something) {
                match action {
                    SessionAction::Cut { subscription_uid, username, best_effort } => {
                        match tak.cut_subscription(&subscription_uid).await {
                            Ok(()) => report.changes.push(Change::SessionCut { username, subscription_uid }),
                            Err(e) if best_effort => tracing::debug!(subscription_uid, error = %e, "group-less session not cut"),
                            Err(e) => report.errors.push(format!("cutting session {subscription_uid}: {e}")),
                        }
                    }
                    // Already refreshed above; the session will follow within a second.
                    SessionAction::Refresh { username } if touched.contains(&username) => {}
                    SessionAction::Refresh { username } => match tak.refresh_groups(&username).await {
                        Ok(()) => report.changes.push(Change::GroupsRefreshed { username }),
                        Err(e) => report.errors.push(format!("refreshing the groups of {username}: {e}")),
                    },
                }
            }
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::directory::memory::MemoryDirectory;
    use crate::directory::{DirectoryGroup, DirectoryUser};
    use crate::tak::memory::MemoryTakServer;

    fn account(username: &str, groups: &[&str]) -> DesiredAccount {
        DesiredAccount {
            username: username.into(),
            password: format!("pw-{username}"),
            password_tag: format!("tag-{username}"),
            groups: groups.iter().map(|g| g.to_string()).collect(),
        }
    }

    fn desired(accounts: Vec<DesiredAccount>) -> DesiredState {
        DesiredState { accounts, ..Default::default() }
    }

    fn two_teams() -> DesiredState {
        desired(vec![account("alpha-shared", &["alpha"]), account("bravo-ann", &["bravo"]), account("bravo-bob", &["bravo"])])
    }

    #[tokio::test]
    async fn first_pass_creates_everything_and_second_pass_changes_nothing() {
        let (dir, tak) = (MemoryDirectory::new(), MemoryTakServer::new());
        let want = two_teams();

        let first = run_pass(&want, &dir, &tak).await;
        assert!(first.errors.is_empty(), "{:?}", first.errors);
        assert_eq!(first.changes.len(), 6, "three accounts, three memberships");
        let state = dir.state();
        assert_eq!(state.users.len(), 3);
        assert_eq!(state.groups["bravo"].members.len(), 2);
        assert_eq!(dir.password_of("alpha-shared").as_deref(), Some("pw-alpha-shared"));
        // Accounts exist before they join a group.
        let writes = dir.writes();
        let pos = |w: &str| writes.iter().position(|x| x == w).unwrap();
        assert!(pos("create_user bravo-bob") < pos("add_member bravo bravo-bob"));
        // Every membership change is pushed to TAK Server.
        for user in ["alpha-shared", "bravo-ann", "bravo-bob"] {
            assert!(tak.calls().contains(&format!("refresh_groups {user}")));
        }

        dir.clear_writes();
        tak.clear_calls();
        let second = run_pass(&want, &dir, &tak).await;
        assert!(second.is_clean(), "{second:?}");
        assert!(dir.writes().is_empty(), "idempotent: {:?}", dir.writes());
        assert!(!tak.calls().iter().any(|c| c.starts_with("refresh_groups")));
    }

    #[tokio::test]
    async fn drift_in_the_directory_is_repaired() {
        let (dir, tak) = (MemoryDirectory::new(), MemoryTakServer::new());
        let want = two_teams();
        run_pass(&want, &dir, &tak).await;

        dir.tamper(|s| {
            // A member removed by hand, an intruder added, a password changed, a stray account
            // and a stray group.
            s.groups.get_mut("bravo").unwrap().members.remove("bravo-bob");
            s.groups.get_mut("alpha").unwrap().members.insert("bravo-ann".into());
            s.groups.get_mut("alpha").unwrap().foreign_members.insert("cn=takserver,ou=Services,dc=tak,dc=example".into());
            s.users.get_mut("alpha-shared").unwrap().password_tag = Some("other".into());
            s.users.insert("intruder".into(), DirectoryUser::default());
            s.groups.insert("ghosts".into(), DirectoryGroup { members: ["intruder".to_string()].into(), ..Default::default() });
        });

        let report = run_pass(&want, &dir, &tak).await;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        let kinds: BTreeSet<&str> = report.changes.iter().map(Change::kind).collect();
        assert_eq!(kinds, ["member_added", "member_removed", "foreign_member_removed", "password_set", "user_deleted"].into());
        let state = dir.state();
        assert_eq!(state.groups["alpha"].members, ["alpha-shared".to_string()].into());
        assert!(state.groups["alpha"].foreign_members.is_empty());
        assert!(state.groups["bravo"].members.contains("bravo-bob"));
        assert!(!state.users.contains_key("intruder"));
        assert!(!state.groups.contains_key("ghosts"), "a group disappears with its last member");
        assert_eq!(state.users["alpha-shared"].password_tag.as_deref(), Some("tag-alpha-shared"));
        // The intruder left its group before being deleted.
        let writes = dir.writes();
        let pos = |w: &str| writes.iter().position(|x| x == w).unwrap();
        assert!(pos("remove_member ghosts intruder") < pos("delete_user intruder"));

        assert!(run_pass(&want, &dir, &tak).await.is_clean());
    }

    #[tokio::test]
    async fn removing_an_access_revokes_before_deleting() {
        let (dir, tak) = (MemoryDirectory::new(), MemoryTakServer::new());
        run_pass(&two_teams(), &dir, &tak).await;
        tak.add_certificate("bravo-bob", "f1");
        tak.add_certificate("bravo-bob", "f2");
        tak.add_certificate("bravo-ann", "f3");
        tak.add_subscription("tls:1", "bravo-bob", &["bravo"]);

        let want = desired(vec![account("alpha-shared", &["alpha"]), account("bravo-ann", &["bravo"])]);
        tak.clear_calls();
        dir.clear_writes();
        let report = run_pass(&want, &dir, &tak).await;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.revoked.len(), 2);
        assert!(!dir.state().users.contains_key("bravo-bob"));
        assert_eq!(tak.certificates().len(), 1, "ann keeps her certificate");
        assert!(tak.subscription_uids().is_empty(), "revocation closed bob's session");
        assert_eq!(report.active_certificates.unwrap().iter().map(|c| c.fingerprint.as_str()).collect::<Vec<_>>(), vec!["f3"]);
    }

    #[tokio::test]
    async fn account_deletion_waits_for_tak_server() {
        let (dir, tak) = (MemoryDirectory::new(), MemoryTakServer::new());
        run_pass(&two_teams(), &dir, &tak).await;
        tak.add_certificate("bravo-bob", "f1");
        let want = desired(vec![account("alpha-shared", &["alpha"]), account("bravo-ann", &["bravo"])]);

        tak.set_unavailable(true);
        let report = run_pass(&want, &dir, &tak).await;
        assert!(!report.tak_reachable);
        assert!(report.errors.iter().any(|e| e.contains("postponed")), "{:?}", report.errors);
        let state = dir.state();
        assert!(state.users.contains_key("bravo-bob"), "not deleted while its certificate may still work");
        assert!(!state.groups["bravo"].members.contains("bravo-bob"), "but already out of its group");

        tak.set_unavailable(false);
        let report = run_pass(&want, &dir, &tak).await;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(!dir.state().users.contains_key("bravo-bob"));
        assert!(tak.certificates().is_empty());
    }

    #[tokio::test]
    async fn requested_revocations_only_hit_the_named_certificates() {
        let (dir, tak) = (MemoryDirectory::new(), MemoryTakServer::new());
        let mut want = two_teams();
        run_pass(&want, &dir, &tak).await;
        tak.add_certificate("alpha-shared", "old");
        tak.add_certificate("alpha-shared", "new");
        want.revoke_fingerprints.insert("old".into());

        let report = run_pass(&want, &dir, &tak).await;
        assert_eq!(report.revoked, vec!["old".to_string()]);
        assert_eq!(tak.certificates().iter().map(|c| c.fingerprint.as_str()).collect::<Vec<_>>(), vec!["new"]);
        assert_eq!(report.changes[0].kind(), "certificate_revoked");
    }

    #[tokio::test]
    async fn protected_usernames_are_left_alone() {
        let (dir, tak) = (MemoryDirectory::new(), MemoryTakServer::new());
        let mut want = two_teams();
        want.protected_usernames.insert("teamtak".into());
        tak.add_certificate("teamtak", "api");
        tak.add_subscription("tls:9", "teamtak", &["whatever"]);
        let report = run_pass(&want, &dir, &tak).await;
        assert!(report.revoked.is_empty());
        assert_eq!(tak.certificates().len(), 1);
        assert_eq!(tak.subscription_uids(), vec!["tls:9".to_string()]);
    }

    #[tokio::test]
    async fn a_session_left_with_no_group_is_cut() {
        let (dir, tak) = (MemoryDirectory::new(), MemoryTakServer::new());
        run_pass(&two_teams(), &dir, &tak).await;
        tak.add_certificate("bravo-ann", "f3");
        tak.add_subscription("tls:1", "bravo-ann", &["bravo"]);
        tak.add_subscription("tls:2", "alpha-shared", &["alpha"]);

        // Team bravo is suspended: its accounts stay but lose their only group.
        let want = desired(vec![account("alpha-shared", &["alpha"]), account("bravo-ann", &[]), account("bravo-bob", &[])]);
        let report = run_pass(&want, &dir, &tak).await;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(report.changes.contains(&Change::SessionCut { username: "bravo-ann".into(), subscription_uid: "tls:1".into() }));
        assert_eq!(tak.subscription_uids(), vec!["tls:2".to_string()]);
        assert!(dir.state().users.contains_key("bravo-ann"), "the account itself stays");
        assert!(!dir.state().groups.contains_key("bravo"));
        assert_eq!(tak.certificates().len(), 1, "no certificate is revoked by a suspension");

        // A client that reconnects has no group: nothing to cut, the pass is clean.
        tak.add_subscription("tls:3", "bravo-ann", &[]);
        assert!(run_pass(&want, &dir, &tak).await.is_clean());

        // The team is reactivated: the session opened without a group would never receive
        // it, so it is cut and the client reconnects with its group.
        let report = run_pass(&two_teams(), &dir, &tak).await;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(report.changes.contains(&Change::SessionCut { username: "bravo-ann".into(), subscription_uid: "tls:3".into() }));
        assert_eq!(tak.subscription_uids(), vec!["tls:2".to_string()], "alpha is untouched");
        assert!(dir.state().groups["bravo"].members.contains("bravo-ann"));
    }

    #[tokio::test]
    async fn groupless_sessions_without_a_username_are_cut_only_when_something_changed() {
        let (dir, tak) = (MemoryDirectory::new(), MemoryTakServer::new());
        let want = two_teams();
        run_pass(&want, &dir, &tak).await;
        // As TAK Server 5.8 lists a session that has no group: no username at all.
        tak.add_subscription("tls:7", "", &[]);
        tak.add_subscription("tls:8", "alpha-shared", &["alpha"]);

        // Nothing changed: the session is left alone, pass after pass (a device of a
        // suspended team may sit there; cutting it every minute would serve nothing).
        assert!(run_pass(&want, &dir, &tak).await.is_clean());
        assert_eq!(tak.subscription_uids().len(), 2);

        // An account joins a group: any group-less session might be one of its devices,
        // which would never receive the group. All of them are cut and reconnect.
        let mut more = want.clone();
        more.accounts.push(account("alpha-new", &["alpha"]));
        let report = run_pass(&more, &dir, &tak).await;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(report.changes.iter().any(|c| c.kind() == "session_cut"));
        assert_eq!(tak.subscription_uids(), vec!["tls:8".to_string()], "sessions that have their groups are untouched");
    }

    #[tokio::test]
    async fn an_event_group_comes_and_goes_without_cutting_anyone() {
        let (dir, tak) = (MemoryDirectory::new(), MemoryTakServer::new());
        let outside = two_teams();
        run_pass(&outside, &dir, &tak).await;
        tak.add_subscription("tls:1", "alpha-shared", &["alpha"]);
        tak.add_subscription("tls:2", "bravo-ann", &["bravo"]);
        // A device of a suspended team, as TAK Server lists it: no username, no group.
        tak.add_subscription("tls:9", "", &[]);
        tak.clear_calls();
        dir.clear_writes();

        // The event starts: both teams get one more group.
        let during = desired(vec![
            account("alpha-shared", &["alpha", "evt-exercise"]),
            account("bravo-ann", &["bravo", "evt-exercise"]),
            account("bravo-bob", &["bravo", "evt-exercise"]),
        ]);
        let report = run_pass(&during, &dir, &tak).await;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.changes.len(), 3, "three memberships and nothing else: {:?}", report.changes);
        assert!(report.changes.iter().all(|c| c.kind() == "member_added" && c.group() == Some("evt-exercise")));
        assert_eq!(dir.state().groups["evt-exercise"].members.len(), 3);
        // Connected clients are told at once, and nobody is disconnected: every session
        // keeps a group before and after, so TAK Server applies the change by itself.
        for user in ["alpha-shared", "bravo-ann", "bravo-bob"] {
            assert!(tak.calls().contains(&format!("refresh_groups {user}")), "{user}");
        }
        assert_eq!(tak.subscription_uids().len(), 3, "no session is cut, not even the group-less one");
        assert!(!tak.calls().iter().any(|c| c.starts_with("cut_subscription")), "{:?}", tak.calls());

        // The event ends: the group empties and disappears; again nobody is cut.
        tak.clear_calls();
        let report = run_pass(&outside, &dir, &tak).await;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(report.changes.iter().all(|c| c.kind() == "member_removed" && c.group() == Some("evt-exercise")));
        assert_eq!(report.changes.len(), 3);
        assert!(!dir.state().groups.contains_key("evt-exercise"), "no group and no member left behind");
        assert_eq!(dir.state().groups["bravo"].members.len(), 2, "team groups are untouched");
        assert_eq!(tak.subscription_uids().len(), 3);
        assert!(run_pass(&outside, &dir, &tak).await.is_clean());
    }

    #[tokio::test]
    async fn a_suspended_participant_loses_the_event_group_with_its_own() {
        let (dir, tak) = (MemoryDirectory::new(), MemoryTakServer::new());
        let during = desired(vec![
            account("alpha-shared", &["alpha", "evt-exercise"]),
            account("bravo-ann", &["bravo", "evt-exercise"]),
        ]);
        run_pass(&during, &dir, &tak).await;
        tak.add_subscription("tls:1", "alpha-shared", &["alpha", "evt-exercise"]);
        tak.add_subscription("tls:2", "bravo-ann", &["bravo", "evt-exercise"]);

        // Bravo is suspended during the event: no group at all, so its session is cut.
        let suspended =
            desired(vec![account("alpha-shared", &["alpha", "evt-exercise"]), account("bravo-ann", &[])]);
        let report = run_pass(&suspended, &dir, &tak).await;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(dir.state().groups["evt-exercise"].members, ["alpha-shared".to_string()].into());
        assert!(report.changes.contains(&Change::SessionCut { username: "bravo-ann".into(), subscription_uid: "tls:2".into() }));
        assert_eq!(tak.subscription_uids(), vec!["tls:1".to_string()], "the host stays connected");
    }

    #[tokio::test]
    async fn a_session_with_stale_groups_is_refreshed() {
        let (dir, tak) = (MemoryDirectory::new(), MemoryTakServer::new());
        let want = two_teams();
        run_pass(&want, &dir, &tak).await;
        tak.add_subscription("tls:1", "bravo-ann", &["bravo", "evt-old"]);
        tak.clear_calls();
        let report = run_pass(&want, &dir, &tak).await;
        assert_eq!(report.changes, vec![Change::GroupsRefreshed { username: "bravo-ann".into() }]);
        assert!(tak.calls().contains(&"refresh_groups bravo-ann".to_string()));
        assert_eq!(tak.subscription_uids().len(), 1, "not cut: it still has a legitimate group");
    }

    #[tokio::test]
    async fn directory_down_is_reported_and_nothing_is_guessed() {
        let (dir, tak) = (MemoryDirectory::new(), MemoryTakServer::new());
        dir.set_unavailable(true);
        let report = run_pass(&two_teams(), &dir, &tak).await;
        assert!(!report.directory_reachable);
        assert!(report.tak_reachable);
        assert_eq!(report.errors.len(), 1);
        assert!(report.changes.is_empty());
    }

    #[tokio::test]
    async fn an_automatic_pass_refuses_a_mass_deletion() {
        let (dir, tak) = (MemoryDirectory::new(), MemoryTakServer::new());
        run_pass(&two_teams(), &dir, &tak).await;
        tak.add_certificate("bravo-ann", "f1");
        dir.clear_writes();

        // The database comes back empty (bad restore): an automatic pass must not wipe TAK.
        let empty = DesiredState::default();
        let guarded = PassOptions { max_unknown_accounts: Some(2) };
        let report = run_pass_with(&empty, &dir, &tak, guarded).await;
        assert_eq!(report.errors.len(), 1);
        assert!(report.changes.is_empty());
        assert!(dir.writes().is_empty());
        assert_eq!(dir.state().users.len(), 3);
        assert_eq!(tak.certificates().len(), 1, "no certificate revoked either");

        // One or two stray accounts are ordinary drift and are removed.
        let one_left = desired(vec![account("alpha-shared", &["alpha"])]);
        let report = run_pass_with(&one_left, &dir, &tak, guarded).await;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(dir.state().users.len(), 1);

        // An administrator can force it.
        let report = run_pass(&empty, &dir, &tak).await;
        assert!(report.errors.is_empty());
        assert!(dir.state().users.is_empty());
    }

    #[test]
    fn every_change_kind_is_listed() {
        let u = || "u".to_string();
        let all = [
            Change::UserCreated { username: u() },
            Change::PasswordSet { username: u() },
            Change::MemberAdded { group: u(), username: u() },
            Change::MemberRemoved { group: u(), username: u() },
            Change::ForeignMemberRemoved { group: u(), member: u() },
            Change::UserDeleted { username: u() },
            Change::CertificateRevoked { username: u(), fingerprint: u(), reason: RevocationReason::Requested },
            Change::CertificateRevoked { username: u(), fingerprint: u(), reason: RevocationReason::NoAccess },
            Change::SessionCut { username: u(), subscription_uid: u() },
            Change::GroupsRefreshed { username: u() },
        ];
        let kinds: BTreeSet<&str> = all.iter().map(Change::kind).collect();
        assert_eq!(kinds, CHANGE_KINDS.iter().copied().collect::<BTreeSet<_>>());
    }

    #[test]
    fn plan_orders_actions() {
        let mut actual = DirectorySnapshot::default();
        actual.users.insert("old".into(), DirectoryUser::default());
        actual.groups.insert("alpha".into(), DirectoryGroup { members: ["old".to_string()].into(), ..Default::default() });
        let plan = plan_directory(&desired(vec![account("new", &["alpha"])]), &actual);
        assert_eq!(
            plan,
            vec![
                DirectoryAction::CreateUser { username: "new".into() },
                DirectoryAction::AddMember { group: "alpha".into(), username: "new".into() },
                DirectoryAction::RemoveMember { group: "alpha".into(), username: "old".into() },
                DirectoryAction::DeleteUser { username: "old".into() },
            ],
            "the newcomer joins before the last old member leaves, so the group never empties"
        );
    }
}
