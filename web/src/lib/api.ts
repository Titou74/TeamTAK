// The only place that talks to the API. Errors carry the API's stable code; the UI
// translates it (`errors.<code>`), it never shows a sentence coming from the server.
import type {
  AlertsSummary,
  AuditFilter,
  AuditPage,
  BackupRun,
  Devices,
  History,
  Monitoring,
  Release,
  Settings,
  SettingsView,
} from "./monitoring-types";
import type { Theme } from "./theme";
import type {
  AccessDetail,
  AccessKind,
  AdminAccount,
  AdminTeam,
  AdminTeams,
  InvitableTeam,
  LoginChallenge,
  LoginProof,
  Me,
  PublicConfig,
  Reconciliation,
  Team,
  TeamDetail,
  TeamEvent,
  TeamEvents,
  TeamMode,
  TeamRole,
} from "./types";

export class ApiError extends Error {
  readonly code: string;
  readonly status: number;
  /** Seconds after which the same request will be accepted, when the API says so. */
  readonly retryAfter: number | null;

  constructor(code: string, status: number, retryAfter: number | null = null) {
    super(code);
    this.code = code;
    this.status = status;
    this.retryAfter = retryAfter;
  }
}

async function request(method: string, path: string, body?: unknown): Promise<Response> {
  let response: Response;
  try {
    response = await fetch(`/api${path}`, {
      method,
      credentials: "same-origin",
      headers: {
        Accept: "application/json",
        // Required by the API on every state-changing request (CSRF protection).
        "X-Requested-With": "teamtak",
        ...(body === undefined ? {} : { "Content-Type": "application/json" }),
      },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
  } catch {
    throw new ApiError("network", 0);
  }
  if (!response.ok) {
    let code = "unknown";
    let retryAfter: number | null = null;
    try {
      const data = (await response.json()) as { error?: string; retry_after?: number };
      if (typeof data.error === "string") code = data.error;
      if (typeof data.retry_after === "number") retryAfter = data.retry_after;
    } catch {
      // Not JSON (proxy error page): keep "unknown".
    }
    throw new ApiError(code, response.status, retryAfter);
  }
  return response;
}

async function json<T>(method: string, path: string, body?: unknown): Promise<T> {
  const response = await request(method, path, body);
  if (response.status === 204) return undefined as T;
  return (await response.json()) as T;
}

export interface Synced {
  synced: boolean;
}

export const api = {
  config: () => json<PublicConfig>("GET", "/config"),
  me: () => json<Me>("GET", "/me"),
  loginChallenge: () => json<LoginChallenge>("GET", "/auth/challenge"),
  requestCode: (email: string, language: string, proof: LoginProof) =>
    json<void>("POST", "/auth/request-code", { email, language, proof }),
  verifyCode: (email: string, code: string, language: string) => json<Me>("POST", "/auth/verify", { email, code, language }),
  logout: () => json<void>("POST", "/auth/logout"),
  updateMe: (changes: { display_name?: string; language?: string; theme?: Theme }) => json<Me>("PATCH", "/me", changes),

  requestTeam: (name: string, mode: TeamMode, message: string) => json<Team>("POST", "/teams", { name, mode, message }),
  team: (id: string) => json<TeamDetail>("GET", `/teams/${id}`),
  setTeamMode: (id: string, mode: TeamMode) =>
    json<Synced & { removed_accesses: number }>("PATCH", `/teams/${id}`, { mode }),
  deleteTeam: (id: string) => json<Synced>("DELETE", `/teams/${id}`),
  invite: (teamId: string, email: string, language: string) =>
    json<{ id: string; email: string; email_sent: boolean }>("POST", `/teams/${teamId}/invitations`, { email, language }),
  cancelInvitation: (teamId: string, invitationId: string) =>
    json<void>("DELETE", `/teams/${teamId}/invitations/${invitationId}`),
  answerInvitation: (invitationId: string, accept: boolean) =>
    json<{ team_id: string; accepted: boolean }>("POST", `/invitations/${invitationId}/answer`, { accept }),
  removeMember: (teamId: string, accountId: string) => json<Synced>("DELETE", `/teams/${teamId}/members/${accountId}`),
  setMemberRole: (teamId: string, accountId: string, role: TeamRole) =>
    json<void>("PATCH", `/teams/${teamId}/members/${accountId}`, { role }),

  createAccess: (teamId: string, body: { kind: AccessKind; account_id?: string; label?: string }) =>
    json<Synced & { id: string; username: string }>("POST", `/teams/${teamId}/accesses`, body),
  access: (id: string) => json<AccessDetail>("GET", `/accesses/${id}`),
  regenerateAccess: (id: string) => json<{ id: string; reenrolled: boolean }>("POST", `/accesses/${id}/regenerate`),
  deleteAccess: (id: string) => json<Synced>("DELETE", `/accesses/${id}`),
  /** Downloads a connection zip and returns it with the file name chosen by the server. */
  /** A connection zip; `port` is one of the access's `server.ports`, the main one when absent. */
  accessPackage: async (id: string, form: "enrollment" | "certificate", port?: number) => {
    const query = port === undefined ? "" : `?port=${port}`;
    const response = await request("GET", `/accesses/${id}/package/${form}${query}`);
    const disposition = response.headers.get("Content-Disposition") ?? "";
    const name = /filename="([^"]+)"/.exec(disposition)?.[1] ?? `${form}.zip`;
    return { name, blob: await response.blob() };
  },

  // Cross-team events. Every call names the team the caller acts for.
  teamEvents: (teamId: string) => json<TeamEvents>("GET", `/teams/${teamId}/events`),
  invitableTeams: (teamId: string) => json<{ teams: InvitableTeam[] }>("GET", `/teams/${teamId}/invitable-teams`),
  /** Dates are ISO 8601 in UTC; a missing start means "now". */
  createEvent: (teamId: string, body: { name: string; starts_at?: string; ends_at: string }) =>
    json<Synced & { id: string; group_name: string }>("POST", `/teams/${teamId}/events`, body),
  updateEvent: (teamId: string, eventId: string, dates: { starts_at?: string; ends_at?: string }) =>
    json<Synced>("PATCH", `/teams/${teamId}/events/${eventId}`, dates),
  endEvent: (teamId: string, eventId: string) => json<Synced>("POST", `/teams/${teamId}/events/${eventId}/end`),
  cancelEvent: (teamId: string, eventId: string) => json<Synced>("POST", `/teams/${teamId}/events/${eventId}/cancel`),
  inviteTeamToEvent: (teamId: string, eventId: string, invitedTeamId: string) =>
    json<{ team_id: string; name: string }>("POST", `/teams/${teamId}/events/${eventId}/teams`, { team_id: invitedTeamId }),
  removeTeamFromEvent: (teamId: string, eventId: string, otherTeamId: string) =>
    json<Synced>("DELETE", `/teams/${teamId}/events/${eventId}/teams/${otherTeamId}`),
  answerEvent: (teamId: string, eventId: string, accept: boolean) =>
    json<Synced & { accepted: boolean }>("POST", `/teams/${teamId}/events/${eventId}/answer`, { accept }),
  leaveEvent: (teamId: string, eventId: string) => json<Synced>("POST", `/teams/${teamId}/events/${eventId}/leave`),

  adminTeams: () => json<AdminTeams>("GET", "/admin/teams"),
  adminEvents: () => json<{ events: TeamEvent[] }>("GET", "/admin/events"),
  adminCancelEvent: (eventId: string) => json<Synced>("POST", `/admin/events/${eventId}/cancel`),
  decideTeam: (id: string, approve: boolean, options: { member_cap?: number; note?: string }) =>
    json<Team>("POST", `/admin/teams/${id}/decision`, { approve, ...options }),
  adminUpdateTeam: (id: string, changes: { member_cap?: number; suspended?: boolean }) =>
    json<Synced & { team: AdminTeam }>("PATCH", `/admin/teams/${id}`, changes),
  adminAccounts: () => json<{ accounts: AdminAccount[] }>("GET", "/admin/accounts"),
  setAdmin: (id: string, isAdmin: boolean) => json<void>("PATCH", `/admin/accounts/${id}`, { is_admin: isAdmin }),
  reconciliation: () => json<Reconciliation>("GET", "/admin/reconciliation"),
  runReconciliation: () => json<Reconciliation>("POST", "/admin/reconciliation/run"),

  // Monitoring (administrators). What is shown is what the platform last saw; `refresh`
  // makes it look first.
  monitoring: (refresh = false) => json<Monitoring>("GET", `/admin/monitoring${refresh ? "?refresh=true" : ""}`),
  monitoringHistory: (hours: number) => json<History>("GET", `/admin/monitoring/history?hours=${hours}`),
  alertsSummary: () => json<AlertsSummary>("GET", "/admin/alerts/summary"),
  adminDevices: () => json<Devices>("GET", "/admin/devices"),
  teamDevices: (teamId: string) => json<Devices>("GET", `/teams/${teamId}/devices`),
  restartService: (service: string) => json<{ restarted: boolean; accepted: boolean }>("POST", `/admin/services/${service}/restart`),
  runBackup: () => json<{ run: BackupRun }>("POST", "/admin/backups/run"),
  syncLetsEncrypt: () => json<{ outcome: string }>("POST", "/admin/letsencrypt/sync"),
  /** Asks GitHub for TeamTAK's latest release now. GitHub's refusal is in `release.error`. */
  checkRelease: () => json<{ release: Release }>("POST", "/admin/release/check"),
  settings: () => json<SettingsView>("GET", "/admin/settings"),
  /** Only the fields given change. `backup_passphrase`: a new one, or "" to remove it. */
  updateSettings: (changes: { settings?: DeepPartial<Settings>; backup_passphrase?: string }) =>
    json<SettingsView>("PATCH", "/admin/settings", changes),
  testWebhook: () => json<{ status: number }>("POST", "/admin/settings/webhook-test"),
  /** Sends the test e-mail to the signed-in administrator. `error`: the mail server's refusal, technical (English). */
  testMail: () => json<{ sent: boolean; to: string; error: string | null }>("POST", "/admin/settings/mail-test"),
  audit: (filter: AuditFilter) => {
    const query = new URLSearchParams();
    for (const [key, value] of Object.entries(filter)) {
      if (value !== undefined && value !== "") query.set(key, String(value));
    }
    return json<AuditPage>("GET", `/admin/audit?${query.toString()}`);
  },
};

type DeepPartial<T> = { [K in keyof T]?: T[K] extends object ? DeepPartial<T[K]> : T[K] };
