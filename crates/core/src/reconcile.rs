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
//!
//! The video feeds TAK Server lists follow the same way ([`crate::video::plan_feeds`]): one
//! per valid video stream, for the group of the stream. So do its device profiles
//! ([`crate::maps::plan_profiles`]): one per group that has maps, and the enrollment profile.
//!
//! TAK Server can also keep listing a session whose connection is gone, which no cut removes:
//! see [`settle_failed_cuts`] for how such a leftover is told apart from a real failure.

use std::collections::{BTreeMap, BTreeSet};

use crate::directory::{Directory, DirectorySnapshot};
use crate::maps::{group_of_profile, plan_profiles, DesiredProfile, ProfileAction, PROFILE_PREFIX};
use crate::tak::{IssuedCertificate, Subscription, TakError, TakServer};
use crate::video::{path_of_url, plan_feeds, split_path, DesiredFeed, FeedAction};

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
    /// The video feeds TAK Server must list, one per valid video stream; `None` leaves its
    /// video feeds as they are.
    pub video_feeds: Option<Vec<DesiredFeed>>,
    /// The device profiles TeamTAK keeps in TAK Server (maps of each group, enrollment);
    /// `None` leaves its profiles as they are.
    pub device_profiles: Option<Vec<DesiredProfile>>,
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

/// A cut of a session with an account that TAK Server did not confirm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedCut {
    pub subscription_uid: String,
    pub username: String,
    /// The HTTP status TAK Server answered with; none when it did not answer.
    pub status: Option<u16>,
    pub error: String,
}

impl FailedCut {
    fn message(&self) -> String {
        format!("cutting session {}: {}", self.subscription_uid, self.error)
    }

    fn leftover_message(&self) -> String {
        format!(
            "session {} of {}: TAK Server lists it without any group but answered 400 to the cut, as it does \
             when no connection is behind the entry. It sees nothing and nobody sees it; restarting TAK Server \
             clears it.",
            self.subscription_uid, self.username
        )
    }
}

/// What became of the cuts TAK Server did not confirm.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SettledCuts {
    /// Sessions that are gone after all.
    pub changes: Vec<Change>,
    /// Sessions still there that the platform must still deal with.
    pub errors: Vec<String>,
    /// Leftover entries: still listed, but with nothing behind them.
    pub warnings: Vec<String>,
}

/// What became of the cuts TAK Server did not confirm, given the sessions it lists afterwards.
///
/// TAK Server 5.8 sometimes answers 400 to a cut it has just carried out: the call closes the
/// connection, the handler of the closed connection removes the session, and when it does so
/// first, the call finds nothing left to remove (seen in the integration tests). A session can
/// also close by itself between the listing and the cut. Either way a session that is no longer
/// listed is gone, which is what the cut was for: it counts as cut.
///
/// TAK Server can also go on listing a session whose connection is gone, until it restarts.
/// When a client closes its connection right after its first message, the handler of the
/// closed connection removes the session while another thread is still recording the
/// client's callsign, and that recording puts the session back in the list: TAK Server's
/// messaging log shows `Removed Subscription: tls:1` followed by `Set client for
/// subscription: tls:1` (seen after a restart of TAK Server with the scripted client of an
/// integration test, which disconnects a tenth of a second after connecting, and with clients
/// whose certificate was just revoked, which the server disconnects at once). No TCP connection
/// is open behind such an entry. It is listed without any group, the groups having gone with the
/// connection, sometimes with its account's username, and every cut answers 400 ("Could not
/// delete subscription with uid"), as for any identifier TAK Server has no connection for.
///
/// So a session still listed after a refused cut is:
/// - a leftover when it holds no group and the answer was 400. It sees nothing and nobody sees
///   it, and no cut can remove it. It is a warning, not an error: as an error it would make
///   every change of the platform look unapplied until TAK Server restarts, and finally raise
///   the "synchronisation failing" alert, for something that leaks nothing.
/// - an error otherwise. A session that still holds groups may still see what its account must
///   no longer see. Any other answer (500, no answer) says nothing about the connection, so the
///   session may be a device that is really connected and will never get its groups.
pub fn settle_failed_cuts(failed: Vec<FailedCut>, listed: &[Subscription]) -> SettledCuts {
    let listed: BTreeMap<&str, &Subscription> = listed.iter().map(|s| (s.subscription_uid.as_str(), s)).collect();
    let mut settled = SettledCuts::default();
    for cut in failed {
        match listed.get(cut.subscription_uid.as_str()) {
            None => settled.changes.push(Change::SessionCut { username: cut.username, subscription_uid: cut.subscription_uid }),
            Some(session) if session.groups.is_empty() && cut.status == Some(400) => {
                settled.warnings.push(cut.leftover_message())
            }
            Some(_) => settled.errors.push(cut.message()),
        }
    }
    settled
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
    /// A video stream listed in TAK Server for the members of its group.
    VideoFeedAdded { path: String, group: String },
    /// Its name or address changed (keys regenerated, team renamed).
    VideoFeedUpdated { path: String, group: String },
    /// The stream is gone, or its group is (the event is over, the team suspended).
    VideoFeedRemoved { path: String, group: String },
    /// Devices publishing or watching a stream that is no longer valid were disconnected
    /// from the video server.
    VideoStopped { path: String, group: String },
    /// A device profile was created in TAK Server, with its files: the maps of a group, or
    /// the enrollment profile (`group` is then empty).
    DeviceProfileAdded { name: String, group: String },
    /// Its files, groups or kind changed (maps added or taken off, a team approved).
    DeviceProfileUpdated { name: String, group: String },
    /// The group has no map left, or is gone (the event is over, the team suspended).
    DeviceProfileRemoved { name: String, group: String },
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
    "video_feed_added",
    "video_feed_updated",
    "video_feed_removed",
    "video_stopped",
    "device_profile_added",
    "device_profile_updated",
    "device_profile_removed",
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
            Change::VideoFeedAdded { .. } => "video_feed_added",
            Change::VideoFeedUpdated { .. } => "video_feed_updated",
            Change::VideoFeedRemoved { .. } => "video_feed_removed",
            Change::VideoStopped { .. } => "video_stopped",
            Change::DeviceProfileAdded { .. } => "device_profile_added",
            Change::DeviceProfileUpdated { .. } => "device_profile_updated",
            Change::DeviceProfileRemoved { .. } => "device_profile_removed",
        }
    }

    /// The account concerned (the raw member identifier for a foreign member; the callsign
    /// of the device for a session TAK Server lists without a username); for video, the path
    /// of the stream; for a device profile, its name.
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
            Change::VideoFeedAdded { path, .. }
            | Change::VideoFeedUpdated { path, .. }
            | Change::VideoFeedRemoved { path, .. }
            | Change::VideoStopped { path, .. } => path,
            Change::DeviceProfileAdded { name, .. }
            | Change::DeviceProfileUpdated { name, .. }
            | Change::DeviceProfileRemoved { name, .. } => name,
        }
    }

    /// The group, when the change is about a membership, a video stream or the maps of that
    /// group.
    pub fn group(&self) -> Option<&str> {
        match self {
            Change::MemberAdded { group, .. }
            | Change::MemberRemoved { group, .. }
            | Change::ForeignMemberRemoved { group, .. } => Some(group),
            Change::VideoFeedAdded { group, .. }
            | Change::VideoFeedUpdated { group, .. }
            | Change::VideoFeedRemoved { group, .. }
            | Change::VideoStopped { group, .. }
            | Change::DeviceProfileAdded { group, .. }
            | Change::DeviceProfileUpdated { group, .. }
            | Change::DeviceProfileRemoved { group, .. } => Some(group.as_str()).filter(|g| !g.is_empty()),
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
    /// Anomalies of TAK Server that leave nothing to apply and nothing exposed, in plain
    /// English for the logs. They do not make the pass fail.
    pub warnings: Vec<String>,
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
            let mut failed_cuts = Vec::new();
            for action in plan_sessions(desired, &subscriptions, changed_something) {
                match action {
                    SessionAction::Cut { subscription_uid, username, best_effort } => {
                        match tak.cut_subscription(&subscription_uid).await {
                            Ok(()) => report.changes.push(Change::SessionCut { username, subscription_uid }),
                            Err(e) if best_effort => tracing::debug!(subscription_uid, error = %e, "group-less session not cut"),
                            Err(e) => {
                                tracing::debug!(subscription_uid, error = %e, "cut not confirmed, checking whether the session is gone");
                                let status = match &e {
                                    TakError::Status { status, .. } => Some(*status),
                                    _ => None,
                                };
                                failed_cuts.push(FailedCut { subscription_uid, username, status, error: e.to_string() });
                            }
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
            if !failed_cuts.is_empty() {
                // A refused cut may still have closed the session, or the session may be a
                // leftover entry with nothing behind it: see [`settle_failed_cuts`]. A leftover
                // is asked to be cut again at every pass, which costs two calls. Remembering it
                // from one pass to the next would save them, but TAK Server numbers sessions
                // from tls:1 again when it restarts, and a real session could then be spared.
                match tak.subscriptions().await {
                    Ok(listed) => {
                        let settled = settle_failed_cuts(failed_cuts, &listed);
                        report.changes.extend(settled.changes);
                        report.errors.extend(settled.errors);
                        report.warnings.extend(settled.warnings);
                    }
                    Err(_) => report.errors.extend(failed_cuts.iter().map(FailedCut::message)),
                }
            }
        }
    }

    // 6. Video feeds: one per valid stream, listed to the members of its group only.
    if let Some(feeds) = &desired.video_feeds {
        sync_video_feeds(feeds, tak, &mut report).await;
    }

    // 7. Device profiles: the maps of each group, handed to its devices when they connect.
    if let Some(profiles) = &desired.device_profiles {
        sync_device_profiles(profiles, tak, &mut report).await;
    }
    report
}

/// The change a profile action makes, for the journal: one per profile and pass.
fn profile_change(name: &str, kind: fn(String, String) -> Change) -> Change {
    kind(name.to_string(), group_of_profile(name).unwrap_or_default().to_string())
}

async fn sync_device_profiles(desired: &[DesiredProfile], tak: &dyn TakServer, report: &mut PassReport) {
    let listed = match tak.device_profiles().await {
        Ok(listed) => listed,
        Err(e) => return report.errors.push(format!("listing device profiles: {e}")),
    };
    let mut files = BTreeMap::new();
    let wanted: BTreeSet<&str> = desired.iter().map(|p| p.name.as_str()).collect();
    // Only the files of the profiles that stay are compared; the others go whole.
    for profile in listed.iter().filter(|p| p.name.starts_with(PROFILE_PREFIX) && wanted.contains(p.name.as_str())) {
        match tak.profile_files(&profile.name).await {
            Ok(found) => {
                files.insert(profile.name.clone(), found);
            }
            Err(e) => return report.errors.push(format!("reading the files of device profile {}: {e}", profile.name)),
        }
    }
    // One journal entry per profile, whatever the number of calls it took.
    let mut changes: BTreeMap<String, Change> = BTreeMap::new();
    let updated = |changes: &mut BTreeMap<String, Change>, name: &str| {
        changes.entry(name.to_string()).or_insert_with(|| profile_change(name, |name, group| Change::DeviceProfileUpdated { name, group }));
    };
    let mut created = Vec::new();
    for action in plan_profiles(desired, &listed, &files) {
        match action {
            ProfileAction::Create(want) => {
                let groups: Vec<String> = want.groups.iter().cloned().collect();
                match tak.create_device_profile(&want.name, &groups).await {
                    Ok(()) => created.push(want),
                    Err(e) => report.errors.push(format!("creating device profile {}: {e}", want.name)),
                }
            }
            ProfileAction::Set { id, desired: want } => {
                let groups: Vec<String> = want.groups.iter().cloned().collect();
                match tak.set_device_profile(id, &want.name, want.kind, &groups).await {
                    Ok(()) => updated(&mut changes, &want.name),
                    Err(e) => report.errors.push(format!("setting device profile {}: {e}", want.name)),
                }
            }
            ProfileAction::AddFile { profile, file, data } => match tak.add_profile_file(&profile, &file, &data).await {
                Ok(()) => updated(&mut changes, &profile),
                Err(e) => report.errors.push(format!("adding {file} to device profile {profile}: {e}")),
            },
            ProfileAction::DeleteFile { profile, file, file_id } => match tak.delete_profile_file(&profile, file_id).await {
                Ok(()) => updated(&mut changes, &profile),
                Err(e) => report.errors.push(format!("removing {file} from device profile {profile}: {e}")),
            },
            ProfileAction::Delete { id, name } => match tak.delete_device_profile(id).await {
                Ok(()) => {
                    changes.insert(name.clone(), profile_change(&name, |name, group| Change::DeviceProfileRemoved { name, group }));
                }
                Err(e) => report.errors.push(format!("removing device profile {name}: {e}")),
            },
        }
    }
    if !created.is_empty() {
        // TAK Server answers a creation without the new profile's identifier, and makes it an
        // enrollment profile: it is read back, set as wanted, then given its files.
        match tak.device_profiles().await {
            Err(e) => report.errors.push(format!("listing device profiles: {e}")),
            Ok(relisted) => {
                for want in created {
                    let Some(have) = relisted.iter().find(|p| p.name == want.name) else {
                        report.errors.push(format!("device profile {} not listed after its creation", want.name));
                        continue;
                    };
                    if have.kind != Some(want.kind) || have.groups != want.groups || !have.active {
                        let groups: Vec<String> = want.groups.iter().cloned().collect();
                        if let Err(e) = tak.set_device_profile(have.id, &want.name, want.kind, &groups).await {
                            report.errors.push(format!("setting device profile {}: {e}", want.name));
                        }
                    }
                    for (file, data) in &want.files {
                        if let Err(e) = tak.add_profile_file(&want.name, file, data).await {
                            report.errors.push(format!("adding {file} to device profile {}: {e}", want.name));
                        }
                    }
                    changes.insert(want.name.clone(), profile_change(&want.name, |name, group| Change::DeviceProfileAdded { name, group }));
                }
            }
        }
    }
    report.changes.extend(changes.into_values());
}

async fn sync_video_feeds(desired: &[DesiredFeed], tak: &dyn TakServer, report: &mut PassReport) {
    let listed = match tak.video_feeds().await {
        Ok(listed) => listed,
        Err(e) => return report.errors.push(format!("listing video feeds: {e}")),
    };
    for action in plan_feeds(desired, &listed) {
        let (result, change) = match &action {
            FeedAction::Add(want) => (
                tak.add_video_feed(&want.feed, &want.group).await,
                Change::VideoFeedAdded { path: want.path.clone(), group: want.group.clone() },
            ),
            FeedAction::Update(want) => (
                tak.update_video_feed(&want.feed).await,
                Change::VideoFeedUpdated { path: want.path.clone(), group: want.group.clone() },
            ),
            FeedAction::Remove(have) => {
                let path = path_of_url(&have.url).unwrap_or_else(|| have.uid.clone());
                let group = split_path(&path).map(|(g, _)| g.to_string()).unwrap_or_default();
                (tak.delete_video_feed(&have.uid).await, Change::VideoFeedRemoved { path, group })
            }
        };
        match result {
            Ok(()) => report.changes.push(change),
            Err(e) => report.errors.push(format!("video feed of {}: {e}", change.subject())),
        }
    }
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
    async fn a_session_gone_when_its_cut_fails_counts_as_cut() {
        let (dir, tak) = (MemoryDirectory::new(), MemoryTakServer::new());
        run_pass(&two_teams(), &dir, &tak).await;
        tak.add_subscription("tls:1", "bravo-ann", &["bravo"]);
        tak.add_subscription("tls:2", "bravo-bob", &["bravo"]);
        tak.add_subscription("tls:3", "alpha-shared", &["alpha"]);
        // Team bravo is suspended. Ann's device disconnects by itself right after the pass
        // has listed the sessions, so its cut is answered 400; Bob's session is cut, but TAK
        // Server answers 400 all the same, as it sometimes does.
        tak.close_after_next_listing("tls:1");
        tak.fail_next_cut("tls:2", 400, true);
        tak.clear_calls();

        let suspended = desired(vec![account("alpha-shared", &["alpha"]), account("bravo-ann", &[]), account("bravo-bob", &[])]);
        let report = run_pass(&suspended, &dir, &tak).await;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        for (username, uid) in [("bravo-ann", "tls:1"), ("bravo-bob", "tls:2")] {
            let cut = Change::SessionCut { username: username.into(), subscription_uid: uid.into() };
            assert!(report.changes.contains(&cut), "{:?}", report.changes);
        }
        assert_eq!(tak.subscription_uids(), vec!["tls:3".to_string()]);
        let listings = tak.calls().iter().filter(|c| *c == "subscriptions").count();
        assert_eq!(listings, 2, "listed once more to see what became of the two cuts: {:?}", tak.calls());

        // Nothing left to do: no session is listed again.
        tak.clear_calls();
        assert!(run_pass(&suspended, &dir, &tak).await.is_clean());
        assert_eq!(tak.calls().iter().filter(|c| *c == "subscriptions").count(), 1);
    }

    #[tokio::test]
    async fn a_cut_refused_on_a_session_still_open_stays_an_error() {
        let (dir, tak) = (MemoryDirectory::new(), MemoryTakServer::new());
        run_pass(&two_teams(), &dir, &tak).await;
        tak.add_subscription("tls:1", "bravo-ann", &["bravo"]);
        tak.add_subscription("tls:2", "alpha-shared", &["alpha"]);
        tak.fail_next_cut("tls:1", 500, false);

        let suspended = desired(vec![account("alpha-shared", &["alpha"]), account("bravo-ann", &[]), account("bravo-bob", &[])]);
        let report = run_pass(&suspended, &dir, &tak).await;
        assert_eq!(
            report.errors,
            vec!["cutting session tls:1: TAK Server answered 500 to DELETE /Marti/api/subscriptions/delete/tls:1".to_string()]
        );
        assert!(!report.changes.iter().any(|c| c.kind() == "session_cut"), "{:?}", report.changes);
        assert_eq!(tak.subscription_uids(), vec!["tls:1".to_string(), "tls:2".to_string()]);

        // The next pass cuts it.
        let report = run_pass(&suspended, &dir, &tak).await;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.changes, vec![Change::SessionCut { username: "bravo-ann".into(), subscription_uid: "tls:1".into() }]);
        assert_eq!(tak.subscription_uids(), vec!["tls:2".to_string()]);
    }

    #[test]
    fn a_refused_cut_is_an_error_only_while_the_session_is_listed() {
        let failed =
            |uid: &str| FailedCut { subscription_uid: uid.into(), username: "u".into(), status: Some(500), error: "boom".into() };
        let listed = [Subscription { subscription_uid: "tls:2".into(), username: "u".into(), ..Default::default() }];
        let settled = settle_failed_cuts(vec![failed("tls:1"), failed("tls:2")], &listed);
        assert_eq!(settled.changes, vec![Change::SessionCut { username: "u".into(), subscription_uid: "tls:1".into() }]);
        assert_eq!(settled.errors, vec!["cutting session tls:2: boom".to_string()]);
        assert!(settled.warnings.is_empty());
    }

    #[test]
    fn only_a_400_on_a_session_listed_without_groups_is_a_leftover() {
        let failed = |uid: &str, status: Option<u16>| FailedCut {
            subscription_uid: uid.into(),
            username: "alpha-shared".into(),
            status,
            error: "refused".into(),
        };
        let session = |uid: &str, groups: &[&str]| Subscription {
            subscription_uid: uid.into(),
            username: "alpha-shared".into(),
            groups: groups.iter().map(|g| g.to_string()).collect(),
            ..Default::default()
        };
        let listed = [session("tls:1", &[]), session("tls:2", &["alpha"]), session("tls:3", &[]), session("tls:4", &[])];
        let settled = settle_failed_cuts(
            vec![
                failed("tls:1", Some(400)), // no group, 400: a leftover
                failed("tls:2", Some(400)), // still holds a group: may still see it
                failed("tls:3", Some(500)), // the server failed: says nothing about the connection
                failed("tls:4", None),      // no answer at all
                failed("tls:5", Some(400)), // no longer listed: cut
            ],
            &listed,
        );
        assert_eq!(
            settled.changes,
            vec![Change::SessionCut { username: "alpha-shared".into(), subscription_uid: "tls:5".into() }]
        );
        assert_eq!(settled.warnings.len(), 1, "{:?}", settled.warnings);
        assert!(settled.warnings[0].starts_with("session tls:1 of alpha-shared: "), "{}", settled.warnings[0]);
        assert_eq!(
            settled.errors,
            ["tls:2", "tls:3", "tls:4"].map(|uid| format!("cutting session {uid}: refused")).to_vec(),
            "nothing else is let off"
        );
    }

    #[tokio::test]
    async fn a_leftover_session_is_a_warning_and_does_not_hold_changes_back() {
        let (dir, tak) = (MemoryDirectory::new(), MemoryTakServer::new());
        let want = two_teams();
        run_pass(&want, &dir, &tak).await;
        tak.add_certificate("alpha-shared", "f1");
        // As seen after a restart of TAK Server: a device of alpha disconnected right after its
        // first message, and the server kept its session, with the account but without its group.
        tak.add_leftover("tls:1", "alpha-shared");
        tak.add_subscription("tls:2", "bravo-ann", &["bravo"]);
        tak.clear_calls();

        let report = run_pass(&want, &dir, &tak).await;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
        assert!(report.warnings[0].starts_with("session tls:1 of alpha-shared: "), "{}", report.warnings[0]);
        assert!(report.is_clean(), "nothing was changed and nothing failed: {report:?}");
        assert!(tak.calls().contains(&"cut_subscription tls:1".to_string()), "the cut was tried: {:?}", tak.calls());
        assert_eq!(tak.subscription_uids(), vec!["tls:1".to_string(), "tls:2".to_string()]);

        // A change made meanwhile on the platform is applied, and reported so.
        let mut more = want.clone();
        more.accounts.push(account("alpha-new", &["alpha"]));
        tak.clear_calls();
        let report = run_pass(&more, &dir, &tak).await;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(report.changes.contains(&Change::UserCreated { username: "alpha-new".into() }), "{:?}", report.changes);
        assert_eq!(report.warnings.len(), 1);
        // Tried again at every pass: after a restart, TAK Server gives tls:1 to a new session.
        assert!(tak.calls().contains(&"cut_subscription tls:1".to_string()));
        assert_eq!(tak.subscription_uids(), vec!["tls:1".to_string(), "tls:2".to_string()], "bravo is untouched");

        // Once its account is gone, the entry matches what the database wants (no group): it is
        // left alone, and the deletion goes through.
        let without_alpha =
            desired(vec![account("alpha-new", &["alpha"]), account("bravo-ann", &["bravo"]), account("bravo-bob", &["bravo"])]);
        tak.clear_calls();
        let report = run_pass(&without_alpha, &dir, &tak).await;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        assert!(report.changes.contains(&Change::UserDeleted { username: "alpha-shared".into() }), "{:?}", report.changes);
        assert!(!tak.calls().iter().any(|c| c.starts_with("cut_subscription")), "{:?}", tak.calls());
        assert!(run_pass(&without_alpha, &dir, &tak).await.is_clean());
    }

    #[tokio::test]
    async fn a_400_on_a_session_that_still_holds_groups_stays_an_error() {
        let (dir, tak) = (MemoryDirectory::new(), MemoryTakServer::new());
        run_pass(&two_teams(), &dir, &tak).await;
        tak.add_subscription("tls:1", "bravo-ann", &["bravo"]);
        tak.fail_next_cut("tls:1", 400, false);

        // Team bravo is suspended; the cut of its session is answered 400, and the session is
        // still listed with the team's group: it may still see the team, which is not a leftover.
        let suspended = desired(vec![account("alpha-shared", &["alpha"]), account("bravo-ann", &[]), account("bravo-bob", &[])]);
        let report = run_pass(&suspended, &dir, &tak).await;
        assert_eq!(
            report.errors,
            vec!["cutting session tls:1: TAK Server answered 400 to DELETE /Marti/api/subscriptions/delete/tls:1".to_string()]
        );
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        assert_eq!(tak.subscription_uids(), vec!["tls:1".to_string()]);
    }

    #[tokio::test]
    async fn a_session_without_groups_that_the_server_fails_to_cut_stays_an_error() {
        let (dir, tak) = (MemoryDirectory::new(), MemoryTakServer::new());
        let want = two_teams();
        run_pass(&want, &dir, &tak).await;
        // A device of alpha connected while its team had no group: it never gets the group
        // unless it reconnects. TAK Server fails on the cut with something other than a 400.
        tak.add_subscription("tls:1", "alpha-shared", &[]);
        tak.fail_next_cut("tls:1", 500, false);

        let report = run_pass(&want, &dir, &tak).await;
        assert_eq!(
            report.errors,
            vec!["cutting session tls:1: TAK Server answered 500 to DELETE /Marti/api/subscriptions/delete/tls:1".to_string()]
        );
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);

        // The next pass cuts it.
        let report = run_pass(&want, &dir, &tak).await;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(
            report.changes,
            vec![Change::SessionCut { username: "alpha-shared".into(), subscription_uid: "tls:1".into() }]
        );
        assert!(tak.subscription_uids().is_empty());
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
            Change::VideoFeedAdded { path: u(), group: u() },
            Change::VideoFeedUpdated { path: u(), group: u() },
            Change::VideoFeedRemoved { path: u(), group: u() },
            Change::VideoStopped { path: u(), group: u() },
            Change::DeviceProfileAdded { name: u(), group: u() },
            Change::DeviceProfileUpdated { name: u(), group: u() },
            Change::DeviceProfileRemoved { name: u(), group: u() },
        ];
        let kinds: BTreeSet<&str> = all.iter().map(Change::kind).collect();
        assert_eq!(kinds, CHANGE_KINDS.iter().copied().collect::<BTreeSet<_>>());
    }

    fn video(stream: &str, group: &str, key: &str) -> DesiredFeed {
        let path = format!("{group}/{stream}");
        DesiredFeed {
            feed: crate::tak::VideoFeed {
                uid: format!("{}{stream}", crate::video::FEED_UID_PREFIX),
                alias: stream.to_string(),
                url: format!("rtsp://view:{key}@tak.example.org:8554/{path}"),
            },
            group: group.into(),
            path,
        }
    }

    #[tokio::test]
    async fn video_feeds_follow_the_valid_streams() {
        let (dir, tak) = (MemoryDirectory::new(), MemoryTakServer::new());
        let mut want = two_teams();
        let foreign = crate::tak::VideoFeed { uid: "gate-camera".into(), alias: "Gate".into(), url: "rtsp://cam/1".into() };
        tak.add_foreign_video_feed(foreign.clone(), "alpha");

        want.video_feeds = Some(vec![video("drone", "alpha", "k1"), video("cam", "evt-exercise", "k2")]);
        let report = run_pass(&want, &dir, &tak).await;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        let added: Vec<(&str, Option<&str>)> =
            report.changes.iter().filter(|c| c.kind() == "video_feed_added").map(|c| (c.subject(), c.group())).collect();
        assert_eq!(added, vec![("alpha/drone", Some("alpha")), ("evt-exercise/cam", Some("evt-exercise"))]);
        let groups: Vec<(String, String)> = tak.video_feeds_by_group().into_iter().map(|(f, g)| (f.uid, g)).collect();
        assert!(groups.contains(&("teamtak-video-drone".into(), "alpha".into())), "{groups:?}");
        assert!(groups.contains(&("teamtak-video-cam".into(), "evt-exercise".into())), "{groups:?}");

        assert!(run_pass(&want, &dir, &tak).await.is_clean(), "nothing to do the second time");

        // Keys regenerated for one stream; the event is over for the other.
        want.video_feeds = Some(vec![video("drone", "alpha", "k3")]);
        let report = run_pass(&want, &dir, &tak).await;
        let kinds: Vec<(&str, &str)> = report.changes.iter().map(|c| (c.kind(), c.subject())).collect();
        assert_eq!(kinds, vec![("video_feed_updated", "alpha/drone"), ("video_feed_removed", "evt-exercise/cam")]);
        assert_eq!(report.changes[1].group(), Some("evt-exercise"), "read back from the address");
        let listed = tak.video_feeds_by_group();
        assert!(listed.iter().any(|(f, _)| f.url.contains("view:k3@")));
        assert!(listed.iter().any(|(f, _)| *f == foreign), "a feed registered by someone else stays");

        // Video left alone: nothing asked of TAK Server's video manager.
        want.video_feeds = None;
        tak.clear_calls();
        run_pass(&want, &dir, &tak).await;
        assert!(!tak.calls().iter().any(|c| c.contains("video")), "{:?}", tak.calls());
    }

    #[tokio::test]
    async fn device_profiles_follow_the_maps_of_each_group() {
        use crate::maps::{enrollment_profile, maps_profile, ENROLLMENT_PROFILE};
        use crate::tak::ProfileKind;
        let (dir, tak) = (MemoryDirectory::new(), MemoryTakServer::new());
        let foreign = tak.add_foreign_profile("hq-settings", &["alpha"]);
        let mut want = two_teams();
        let maps = |entries: &[(&str, &str)]| -> BTreeMap<String, Vec<u8>> {
            entries.iter().map(|(n, d)| (n.to_string(), d.as_bytes().to_vec())).collect()
        };
        want.device_profiles = Some(vec![
            enrollment_profile(["alpha".to_string(), "bravo".to_string()].into()).unwrap(),
            maps_profile("alpha", maps(&[("IGN-Plan.xml", "plan"), ("OpenTopoMap.xml", "otm")])).unwrap(),
            maps_profile("evt-exercise", maps(&[("IGN-Plan.xml", "plan")])).unwrap(),
        ]);
        let report = run_pass(&want, &dir, &tak).await;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        let added: Vec<(&str, Option<&str>)> =
            report.changes.iter().filter(|c| c.kind() == "device_profile_added").map(|c| (c.subject(), c.group())).collect();
        assert_eq!(
            added,
            vec![(ENROLLMENT_PROFILE, None), ("teamtak-maps-alpha", Some("alpha")), ("teamtak-maps-evt-exercise", Some("evt-exercise"))],
            "one entry per profile, whatever the number of files"
        );
        let profiles = tak.profiles();
        let alpha = profiles.iter().find(|(p, _)| p.name == "teamtak-maps-alpha").unwrap();
        assert_eq!(alpha.0.kind, Some(ProfileKind::Connection), "created as an enrollment profile, then set");
        assert_eq!(alpha.0.groups.iter().map(String::as_str).collect::<Vec<_>>(), ["alpha"]);
        assert_eq!(alpha.1.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["IGN-Plan.xml", "OpenTopoMap.xml"]);
        let enrollment = profiles.iter().find(|(p, _)| p.name == ENROLLMENT_PROFILE).unwrap();
        assert_eq!(enrollment.0.kind, Some(ProfileKind::Enrollment));

        assert!(run_pass(&want, &dir, &tak).await.is_clean(), "nothing to do the second time");

        // A map changed, one taken off; the event is over; someone edited a profile in TAK Server.
        want.device_profiles = Some(vec![
            enrollment_profile(["alpha".to_string(), "bravo".to_string()].into()).unwrap(),
            maps_profile("alpha", maps(&[("IGN-Plan.xml", "plan v2")])).unwrap(),
        ]);
        tak.edit_profile(ENROLLMENT_PROFILE, |p, _| {
            p.groups.remove("bravo");
        });
        let report = run_pass(&want, &dir, &tak).await;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        let kinds: Vec<(&str, &str)> = report.changes.iter().map(|c| (c.kind(), c.subject())).collect();
        assert_eq!(
            kinds,
            vec![
                ("device_profile_updated", ENROLLMENT_PROFILE),
                ("device_profile_updated", "teamtak-maps-alpha"),
                ("device_profile_removed", "teamtak-maps-evt-exercise"),
            ]
        );
        assert_eq!(report.changes[2].group(), Some("evt-exercise"), "read back from the name");
        let profiles = tak.profiles();
        let alpha = profiles.iter().find(|(p, _)| p.name == "teamtak-maps-alpha").unwrap();
        assert_eq!(alpha.1.len(), 1);
        assert_eq!(alpha.1[0].data, b"plan v2");
        assert!(profiles.iter().any(|(p, _)| p.id == foreign), "a profile made by someone else stays");
        assert_eq!(profiles.len(), 3);

        // Profiles left alone: nothing asked about them.
        want.device_profiles = None;
        tak.clear_calls();
        run_pass(&want, &dir, &tak).await;
        assert!(!tak.calls().iter().any(|c| c.contains("profile")), "{:?}", tak.calls());
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
