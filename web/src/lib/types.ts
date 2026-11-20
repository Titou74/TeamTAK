// Shapes of the API's JSON answers (crates/server).

import type { Theme } from "./theme";

export type AccessKind = "shared" | "individual" | "external";
/** How a team connects to TAK Server. External accesses can be added in either mode. */
export type TeamMode = Exclude<AccessKind, "external">;
export type TeamStatus = "pending" | "active" | "rejected" | "suspended";
export type TeamRole = "leader" | "member";

export interface Account {
  id: string;
  email: string;
  display_name: string;
  language: string;
  theme: Theme;
  is_admin: boolean;
}

export interface MyTeam {
  id: string;
  name: string;
  slug: string;
  status: TeamStatus;
  mode: TeamMode;
  role: TeamRole;
  decision_note: string;
  /** Invitations to events waiting for an answer; counted for the team's leaders only. */
  event_invitations: number;
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
  /** TAK Server's own administration pages, behind the platform's sign-in: administrators only, null when turned off. */
  tak_admin_url: string | null;
}

export interface PublicConfig {
  languages: string[];
  default_language: string;
  code_ttl_minutes: number;
  /** Seconds before another code can be sent to the same address. */
  resend_cooldown_seconds: number;
  /** Name an administrator gave this server, to tell it from others; empty for none. */
  server_name: string;
}

/** Anti-bot challenge solved before asking for a login code (lib/login-proof.ts). */
export interface LoginChallenge {
  algorithm: string;
  salt: string;
  challenge: string;
  max_number: number;
  signature: string;
  /** Seconds it can be used from now. */
  expires_in: number;
}

/** A solved challenge, sent with the request for a code. */
export interface LoginProof {
  salt: string;
  challenge: string;
  number: number;
  signature: string;
}

export interface Team {
  id: string;
  name: string;
  slug: string;
  status: TeamStatus;
  mode: TeamMode;
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
    /** Leaders (and administrators) see the devices connected with the team's accesses. */
    view_devices: boolean;
  };
}

export type EventPhase = "scheduled" | "running" | "ended" | "cancelled";
/** Where a team stands in an event; "host" is the team that created it. An event organised by the administrators has no host. */
export type EventTeamStatus = "host" | "accepted" | "invited" | "declined" | "left" | "removed";

export interface EventTeam {
  /** Given only to those who run the event (leaders of the host) and to administrators. */
  team_id: string | null;
  name: string;
  status: EventTeamStatus;
}

export interface TeamEvent {
  id: string;
  name: string;
  /** Name of the TAK group: the channel shown in ATAK and iTAK while the event runs. */
  group_name: string;
  starts_at: string;
  ends_at: string;
  cancelled_at: string | null;
  cancel_reason: "host" | "admin" | "host_deleted" | null;
  phase: EventPhase;
  /** "admin": organised by the administrators, with no host team; they put the teams in it directly. */
  organizer: "team" | "admin";
  /** Null when the administrators organise the event. */
  host_team_name: string | null;
  /** How the team the list was asked for relates to the event; null in the administration area. */
  relation: "host" | "invited" | "accepted" | null;
  teams: EventTeam[];
  permissions: { manage: boolean; end: boolean; cancel: boolean; answer: boolean; leave: boolean };
}

export interface TeamEvents {
  events: TeamEvent[];
  permissions: { create: boolean };
}

export interface InvitableTeam {
  id: string;
  name: string;
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
  /** `port` is the main one of `ports`, the ports TAK clients may connect to. */
  server: { host: string; port: number; ports: number[]; enroll_port: number; name: string };
  /** `itak`: one string per client port, keyed by the port. */
  qr: { itak: Record<string, string>; atak: string };
  certificates: CertificateView[];
  can_manage: boolean;
}

export interface AdminTeam {
  id: string;
  name: string;
  slug: string;
  status: TeamStatus;
  mode: TeamMode;
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
  trigger: "change" | "periodic" | "manual" | "startup" | "schedule";
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
