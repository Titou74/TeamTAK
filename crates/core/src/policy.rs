//! Who may do what. Every authorisation decision of the API goes through [`can`], so the
//! rules are in one place and unit-tested.

use crate::names::AccessKind;

/// What the caller is on the platform and, when the action concerns a team, in that team.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Actor {
    pub is_admin: bool,
    pub team_role: Option<TeamRole>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamRole {
    Leader,
    Member,
}

impl TeamRole {
    pub fn as_str(self) -> &'static str {
        match self {
            TeamRole::Leader => "leader",
            TeamRole::Member => "member",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "leader" => Some(TeamRole::Leader),
            "member" => Some(TeamRole::Member),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamStatus {
    /// Requested, waiting for an administrator.
    Pending,
    /// Approved: members and TAK accesses are live.
    Active,
    /// Turned down by an administrator.
    Rejected,
    /// Frozen by an administrator: accounts stay but lose their group and their sessions.
    Suspended,
}

impl TeamStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            TeamStatus::Pending => "pending",
            TeamStatus::Active => "active",
            TeamStatus::Rejected => "rejected",
            TeamStatus::Suspended => "suspended",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(TeamStatus::Pending),
            "active" => Some(TeamStatus::Active),
            "rejected" => Some(TeamStatus::Rejected),
            "suspended" => Some(TeamStatus::Suspended),
            _ => None,
        }
    }
}

/// How the access looked at relates to the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccessRef {
    pub kind: AccessKind,
    /// True when it is the caller's own individual access.
    pub owned_by_actor: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Approve, reject, suspend, set the member cap, see every team and the drift report.
    AdministerPlatform,
    ViewTeam,
    /// Invite, cancel an invitation, remove a member, change a role.
    ManageMembers,
    /// Change the connection mode or delete the team.
    ManageTeam,
    /// See the credentials, QR codes and zips of an access.
    UseAccess(AccessRef),
    CreateAccess(AccessRef),
    /// Regenerate or remove an access.
    ManageAccess(AccessRef),
}

/// The single authorisation rule of the platform.
///
/// - An administrator runs the platform but does not get a team's TAK credentials by that
///   role alone: they are a secret of the team.
/// - A leader manages the team and every access of it.
/// - A member sees the team, uses the shared access, and handles their own individual access.
/// - An account outside the team can do nothing with it.
pub fn can(actor: Actor, action: Action) -> bool {
    use TeamRole::*;
    match action {
        Action::AdministerPlatform => actor.is_admin,
        Action::ViewTeam => actor.is_admin || actor.team_role.is_some(),
        Action::ManageMembers => actor.team_role == Some(Leader),
        Action::ManageTeam => actor.is_admin || actor.team_role == Some(Leader),
        Action::UseAccess(a) => match actor.team_role {
            Some(Leader) => true,
            Some(Member) => a.kind == AccessKind::Shared || (a.kind == AccessKind::Individual && a.owned_by_actor),
            None => false,
        },
        Action::CreateAccess(a) | Action::ManageAccess(a) => match actor.team_role {
            Some(Leader) => true,
            Some(Member) => a.kind == AccessKind::Individual && a.owned_by_actor,
            None => false,
        },
    }
}

/// Which kinds of access a team may hold, given its connection mode. External accesses are
/// allowed in both modes: a guest never shares the team's identity.
pub fn kind_allowed_in_mode(mode: AccessKind, kind: AccessKind) -> bool {
    match kind {
        AccessKind::External => true,
        other => other == mode,
    }
}

/// A team's member cap covers its members and its pending invitations, so a leader cannot
/// promise more seats than the team has.
pub fn seats_left(member_cap: i64, members: i64, pending_invitations: i64) -> i64 {
    (member_cap - members - pending_invitations).max(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADMIN: Actor = Actor { is_admin: true, team_role: None };
    const LEADER: Actor = Actor { is_admin: false, team_role: Some(TeamRole::Leader) };
    const MEMBER: Actor = Actor { is_admin: false, team_role: Some(TeamRole::Member) };
    const OUTSIDER: Actor = Actor { is_admin: false, team_role: None };

    fn access(kind: AccessKind, own: bool) -> AccessRef {
        AccessRef { kind, owned_by_actor: own }
    }

    #[test]
    fn an_account_with_no_team_can_do_nothing() {
        for action in [
            Action::AdministerPlatform,
            Action::ViewTeam,
            Action::ManageMembers,
            Action::ManageTeam,
            Action::UseAccess(access(AccessKind::Shared, false)),
            Action::CreateAccess(access(AccessKind::Individual, true)),
            Action::ManageAccess(access(AccessKind::External, false)),
        ] {
            assert!(!can(OUTSIDER, action), "{action:?}");
        }
    }

    #[test]
    fn administrator_runs_the_platform_but_holds_no_team_secret() {
        assert!(can(ADMIN, Action::AdministerPlatform));
        assert!(can(ADMIN, Action::ViewTeam));
        assert!(can(ADMIN, Action::ManageTeam));
        assert!(!can(ADMIN, Action::ManageMembers));
        assert!(!can(ADMIN, Action::UseAccess(access(AccessKind::Shared, false))));
        assert!(!can(ADMIN, Action::ManageAccess(access(AccessKind::Shared, false))));
    }

    #[test]
    fn leader_manages_team_and_all_accesses() {
        assert!(!can(LEADER, Action::AdministerPlatform));
        assert!(can(LEADER, Action::ManageMembers));
        assert!(can(LEADER, Action::ManageTeam));
        for kind in [AccessKind::Shared, AccessKind::Individual, AccessKind::External] {
            assert!(can(LEADER, Action::UseAccess(access(kind, false))));
            assert!(can(LEADER, Action::CreateAccess(access(kind, false))));
            assert!(can(LEADER, Action::ManageAccess(access(kind, false))));
        }
    }

    #[test]
    fn member_uses_shared_access_and_owns_their_individual_one() {
        assert!(can(MEMBER, Action::ViewTeam));
        assert!(!can(MEMBER, Action::ManageMembers));
        assert!(!can(MEMBER, Action::ManageTeam));
        assert!(can(MEMBER, Action::UseAccess(access(AccessKind::Shared, false))));
        assert!(!can(MEMBER, Action::ManageAccess(access(AccessKind::Shared, false))), "only a leader regenerates");
        assert!(can(MEMBER, Action::UseAccess(access(AccessKind::Individual, true))));
        assert!(can(MEMBER, Action::CreateAccess(access(AccessKind::Individual, true))));
        assert!(can(MEMBER, Action::ManageAccess(access(AccessKind::Individual, true))));
        assert!(!can(MEMBER, Action::UseAccess(access(AccessKind::Individual, false))), "another member's access");
        assert!(!can(MEMBER, Action::UseAccess(access(AccessKind::External, false))));
        assert!(!can(MEMBER, Action::CreateAccess(access(AccessKind::External, false))));
    }

    #[test]
    fn kinds_per_mode() {
        assert!(kind_allowed_in_mode(AccessKind::Shared, AccessKind::Shared));
        assert!(!kind_allowed_in_mode(AccessKind::Shared, AccessKind::Individual));
        assert!(!kind_allowed_in_mode(AccessKind::Individual, AccessKind::Shared));
        assert!(kind_allowed_in_mode(AccessKind::Individual, AccessKind::External));
        assert!(kind_allowed_in_mode(AccessKind::Shared, AccessKind::External));
    }

    #[test]
    fn member_cap_counts_pending_invitations() {
        assert_eq!(seats_left(10, 3, 2), 5);
        assert_eq!(seats_left(10, 8, 2), 0);
        assert_eq!(seats_left(5, 8, 0), 0, "cap lowered below the current size");
    }
}
