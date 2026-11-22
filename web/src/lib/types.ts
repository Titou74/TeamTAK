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
  /** The terms of use in force, and when the account accepted them: null until it does,
   *  and the UI then shows them and nothing else. */
  terms: { version: string; accepted_at: string | null };
}

export interface PublicConfig {
  languages: string[];
  default_language: string;
  code_ttl_minutes: number;
  /** Seconds before another code can be sent to the same address. */
  resend_cooldown_seconds: number;
  /** Name an administrator gave this server, to tell it from others; empty for none. */
  server_name: string;
  /** An address gets an account only with an invitation waiting for it. */
  invitation_only: boolean;
  /** False: a team asked for is active at once, without an administrator. */
  team_approval: boolean;
  /** A code made by the rescue command waits to be used: the sign-in page offers to type it. */
  rescue_code_pending: boolean;
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
  /** Active or suspended teams it is the only leader of: it cannot be deleted while there is one. */
  sole_leader_of: string[];
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

// Video streams (crates/server/src/video.rs).

/** What the video server says of a stream someone publishes to or watches. */
export interface VideoLive {
  /** A device publishes to it. */
  ready: boolean;
  since: string | null;
  readers: number;
  tracks: string[];
  protocol: string | null;
}

/** A stream in the team's list: no key in it. */
export interface VideoStreamSummary {
  id: string;
  label: string;
  name: string;
  path: string;
  team_name: string;
  /** Declared by this team; otherwise another team of the event declared it. */
  own: boolean;
  declared_by_me: boolean;
  created_at: string;
  regenerated_at: string | null;
  /** Its channel exists now: the video server takes its keys and ATAK lists it. */
  valid: boolean;
  live: VideoLive | null;
  can_use: boolean;
  can_manage: boolean;
}

export interface VideoServerAddress {
  host: string;
  rtsp_port: number;
  rtmp_port: number;
  /** UDP. */
  srt_port: number;
}

export interface VideoChannel {
  /** The TAK group: the team's slug or the event's group name. */
  group: string;
  kind: "team" | "event";
  name: string;
  event_id: string | null;
  phase: EventPhase | null;
  streams: VideoStreamSummary[];
}

export type TeamVideo =
  | { enabled: false }
  | {
      enabled: true;
      /** The video server answered: the live state is known. */
      reachable: boolean;
      server: VideoServerAddress;
      channels: VideoChannel[];
      permissions: { create: boolean };
    };

export interface StreamUrls {
  rtsp: string;
  rtmp: string;
  srt: string;
}

export interface VideoStreamDetail {
  id: string;
  label: string;
  name: string;
  path: string;
  channel: { group: string; kind: "team" | "event"; name: string; phase: EventPhase | null };
  valid: boolean;
  live: VideoLive | null;
  server: VideoServerAddress;
  publish: { user: string; key: string; urls: StreamUrls };
  view: { user: string; key: string; urls: StreamUrls };
  created_at: string;
  regenerated_at: string | null;
  can_manage: boolean;
}

// Maps for ATAK (crates/server/src/maps.rs).

/** What the provider of a map's tiles allows, as far as TeamTAK knows (`maps.terms.<code>`). */
export type MapTerms = "open" | "light_use" | "no_offline" | "restricted" | "unknown";

/** A map source of the catalogue. Its file stays on the server: TAK Server hands it to devices. */
export interface MapSource {
  id: string;
  /** Name of the file on the devices. */
  file_name: string;
  /** The name ATAK lists. */
  name: string;
  url: string;
  host: string;
  terms: MapTerms;
  min_zoom: number | null;
  max_zoom: number | null;
  tile_type: string | null;
  /** The team that added it, which alone sees it; null for the administrators' catalogue. */
  team_id: string | null;
  team_name: string | null;
  checked_at: string;
  /** Why the latest check failed; null when it worked. */
  problem: MapProblem | null;
  /** Held back from the devices: it failed twice in a row. */
  broken: boolean;
  updated_at: string;
}

/** Why a file is not a map source, or why a source does not work (`maps.problem.<code>`). */
export interface MapProblem {
  code: string;
  /** The HTTP status, for `http_status`. */
  status: number | null;
}

export interface AdminMapSource extends MapSource {
  /** Teams whose channel has it. */
  teams: number;
  /** Events not over whose channel has it. */
  events: number;
}

export interface MapSuggestion {
  key: string;
  name: string;
  file_name: string;
  host: string;
  terms: MapTerms;
  /** A source with the same file name is in the catalogue. */
  present: boolean;
}

export interface MapCatalogue {
  sources: AdminMapSource[];
  suggestions: MapSuggestion[];
  limits: { upload_bytes: number; files: number; per_channel: number };
}

export interface MapUploadResult {
  /** The file as uploaded, or as named in the zip. */
  file: string;
  outcome: "added" | "updated" | "unchanged" | "rejected";
  /** Why it was rejected. */
  problem?: MapProblem;
  id?: string;
  file_name?: string;
  name?: string;
  host?: string;
  terms?: MapTerms;
}

export interface MapChannel {
  /** The TAK group: the team's slug or the event's group name. */
  group: string;
  kind: "team" | "event";
  name: string;
  event_id: string | null;
  phase: EventPhase | null;
  maps: MapSource[];
  can_edit: boolean;
}

export interface TeamMaps {
  channels: MapChannel[];
  /** The catalogue and the team's own sources, for whoever can change a channel's maps; empty otherwise. */
  catalogue: MapSource[];
  per_channel: number;
  permissions: { add_sources: boolean };
  limits: { upload_bytes: number; files: number };
}

/** The maps of one event, for its organisers. */
export interface ChannelMaps {
  maps: MapSource[];
  catalogue: MapSource[];
  per_channel: number;
}

// TAK Server files of a team (crates/server/src/files.rs).

/** Who sent a file: through TeamTAK (a member), with a TAK access of the team, or another device. */
export interface FileSender {
  kind: "teamtak" | "access" | "device";
  /** A member's name, a TAK username, or a device's. */
  name: string | null;
  /** Sent by this team. */
  team: boolean;
}

export interface TeamDataPackage {
  /** TAK Server's key of this upload. */
  key: number;
  name: string;
  size: number;
  submitted_at: string | null;
  /** The team's channels it is stored for. */
  channels: string[];
  sender: FileSender;
  can_delete: boolean;
}

export interface TeamMissionFile {
  hash: string;
  name: string;
  size: number;
  added_at: string | null;
  sender: FileSender;
  can_remove: boolean;
}

export interface TeamMission {
  name: string;
  description: string;
  channels: string[];
  /** Protected by a password (or on invitation): nothing of it is shown. */
  locked: boolean;
  /** Files can be added and taken out. */
  writable: boolean;
  created_at: string | null;
  /** Null for a locked mission. */
  files: TeamMissionFile[] | null;
}

export interface TeamFiles {
  /** Where a file can be sent: the team's channel and those of the events running. */
  channels: { group: string; kind: "team" | "event"; name: string }[];
  data_packages: TeamDataPackage[];
  missions: TeamMission[];
  max_bytes: number;
  /** The same limit, as the setting gives it. */
  max_mb: number;
}
