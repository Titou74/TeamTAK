// Shapes of the API's JSON answers (crates/server).

export type AccessKind = "shared" | "individual" | "external";
export type TeamStatus = "pending" | "active" | "rejected" | "suspended";
export type TeamRole = "leader" | "member";

export interface Account {
  id: string;
  email: string;
  display_name: string;
  language: string;
  is_admin: boolean;
}

export interface MyTeam {
  id: string;
  name: string;
  slug: string;
  status: TeamStatus;
  mode: AccessKind;
  role: TeamRole;
  decision_note: string;
}

export interface MyInvitation {
  id: string;
  team_id: string;
  team_name: string;
  invited_by: string;
  created_at: string;
}

export interface Me {
  account: Account;
  teams: MyTeam[];
  invitations: MyInvitation[];
}

export interface PublicConfig {
  languages: string[];
  default_language: string;
  code_ttl_minutes: number;
}

export interface Team {
  id: string;
  name: string;
  slug: string;
  status: TeamStatus;
  mode: AccessKind;
  member_cap: number;
  request_message: string;
  decision_note: string;
  requested_by: string;
  created_at: string;
}

export interface Member {
  account_id: string;
  email: string;
  display_name: string;
  role: TeamRole;
  joined_at: string;
}

export interface PendingInvitation {
  id: string;
  email: string;
  created_at: string;
}

export interface AccessSummary {
  id: string;
  kind: AccessKind;
  username: string;
  label: string;
  account_id: string | null;
  generation: number;
  created_at: string;
  regenerated_at: string | null;
  active_certificates: number;
  next_expiry: string | null;
  can_use: boolean;
  can_manage: boolean;
}

export interface TeamDetail {
  team: Team;
  my_role: TeamRole | null;
  members: Member[];
  invitations: PendingInvitation[];
  accesses: AccessSummary[];
  seats_left: number;
  permissions: {
    manage_members: boolean;
    manage_team: boolean;
    create_shared_access: boolean;
    create_external_access: boolean;
    create_own_access: boolean;
    create_member_access: boolean;
  };
}

export interface CertificateView {
  fingerprint: string;
  source: "package" | "device";
  issued_at: string | null;
  expires_at: string | null;
}

export interface AccessDetail {
  id: string;
  team: { id: string; name: string; slug: string; status: TeamStatus };
  kind: AccessKind;
  label: string;
  owner: { email: string; display_name: string } | null;
  username: string;
  password: string;
  generation: number;
  created_at: string;
  regenerated_at: string | null;
  server: { host: string; port: number; enroll_port: number; name: string };
  qr: { itak: string; atak: string };
  certificates: CertificateView[];
  can_manage: boolean;
}

export interface AdminTeam {
  id: string;
  name: string;
  slug: string;
  status: TeamStatus;
  mode: AccessKind;
  member_cap: number;
  request_message: string;
  decision_note: string;
  created_at: string;
  requester_email: string;
  requester_name: string;
  members: number;
  pending_invitations: number;
  accesses: number;
}

export interface AdminTeams {
  teams: AdminTeam[];
  default_member_cap: number;
  max_member_cap: number;
}

export interface AdminAccount {
  id: string;
  email: string;
  display_name: string;
  language: string;
  is_admin: boolean;
  created_at: string;
  last_login_at: string | null;
  teams: number;
}

export interface ReconcileEvent {
  id: number;
  occurred_at: string;
  trigger: "change" | "periodic" | "manual" | "startup";
  kind: string;
  subject: string;
  group_name: string | null;
  is_drift: boolean;
}

export interface Reconciliation {
  last_run_at: string | null;
  last_ok_at: string | null;
  directory_reachable: boolean;
  tak_reachable: boolean;
  errors: string[];
  interval_seconds: number;
  expected_accounts: number;
  expected_groups: number;
  drift_last_7_days: number;
  events: ReconcileEvent[];
}
