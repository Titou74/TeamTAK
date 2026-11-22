// The only place that talks to the API. Errors carry the API's stable code; the UI
// translates it (`errors.<code>`), it never shows a sentence coming from the server.
import type {
  AlertsSummary,
  AuditFilter,
  AuditPage,
  BackupRun,
  Devices,
  History,
  HistoryRange,
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
  ChannelMaps,
  InvitableTeam,
  LoginChallenge,
  LoginProof,
  MapCatalogue,
  MapSource,
  MapUploadResult,
  Me,
  TeamFiles,
  PublicConfig,
  Reconciliation,
  Team,
  TeamDetail,
  TeamEvent,
  TeamEvents,
  TeamMaps,
  TeamMode,
  TeamRole,
  TeamVideo,
  VideoStreamDetail,
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
  // A file goes as it is; anything else as JSON.
  const raw = body instanceof Blob;
  try {
    response = await fetch(`/api${path}`, {
      method,
      credentials: "same-origin",
      headers: {
        Accept: "application/json",
        // Required by the API on every state-changing request (CSRF protection).
        "X-Requested-With": "teamtak",
        ...(body === undefined ? {} : { "Content-Type": raw ? "application/octet-stream" : "application/json" }),
      },
      body: body === undefined ? undefined : raw ? body : JSON.stringify(body),
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

/** A file the server hands over, with the name it chose. */
async function download(path: string, fallback: string): Promise<{ name: string; blob: Blob }> {
  const response = await request("GET", path);
  const disposition = response.headers.get("Content-Disposition") ?? "";
  // The UTF-8 name when the server gives one, else the plain one.
  const encoded = /filename\*=UTF-8''([^;]+)/.exec(disposition)?.[1];
  const name = encoded ? decodeURIComponent(encoded) : (/filename="([^"]+)"/.exec(disposition)?.[1] ?? fallback);
  return { name, blob: await response.blob() };
}

export const api = {
  config: () => json<PublicConfig>("GET", "/config"),
  me: () => json<Me>("GET", "/me"),
  loginChallenge: () => json<LoginChallenge>("GET", "/auth/challenge"),
  requestCode: (email: string, language: string, proof: LoginProof) =>
    json<void>("POST", "/auth/request-code", { email, language, proof }),
  /** `terms_version`: the version of the terms of use the person accepted on the sign-in page. */
  verifyCode: (email: string, code: string, language: string, termsVersion: string) =>
    json<Me>("POST", "/auth/verify", { email, code, language, terms_version: termsVersion }),
  logout: () => json<void>("POST", "/auth/logout"),
  updateMe: (changes: { display_name?: string; language?: string; theme?: Theme }) => json<Me>("PATCH", "/me", changes),
  acceptTerms: (version: string) => json<Me>("POST", "/me/terms", { version }),

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
  accessPackage: (id: string, form: "enrollment" | "certificate", port?: number) =>
    download(`/accesses/${id}/package/${form}${port === undefined ? "" : `?port=${port}`}`, `${form}.zip`),

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

  // Video streams of a team's channel and of the events it takes part in.
  teamVideo: (teamId: string) => json<TeamVideo>("GET", `/teams/${teamId}/video`),
  /** `event_id`: the event whose channel the stream is for; none for the team's own. */
  createVideoStream: (teamId: string, body: { label: string; event_id?: string }) =>
    json<Synced & { id: string; path: string }>("POST", `/teams/${teamId}/video`, body),
  videoStream: (id: string) => json<VideoStreamDetail>("GET", `/video-streams/${id}`),
  regenerateVideoStream: (id: string) => json<Synced>("POST", `/video-streams/${id}/regenerate`),
  deleteVideoStream: (id: string) => json<Synced>("DELETE", `/video-streams/${id}`),

  // TAK Server files of a team's channels: data packages and Data Sync missions.
  teamFiles: (teamId: string) => json<TeamFiles>("GET", `/teams/${teamId}/files`),
  /** Sends a file to a channel of the team (a group); a file that is not a data package is wrapped in one. */
  uploadTeamFile: (teamId: string, channel: string, file: File) =>
    json<{ key: number | null; name: string; hash: string; wrapped: boolean }>(
      "POST",
      `/teams/${teamId}/files?${new URLSearchParams({ name: file.name, channel }).toString()}`,
      file,
    ),
  deleteTeamFile: (teamId: string, key: number) => json<{ deleted: boolean }>("DELETE", `/teams/${teamId}/files/${key}`),
  teamFileContent: (teamId: string, key: number) => download(`/teams/${teamId}/files/${key}/content`, "file"),
  uploadMissionFile: (teamId: string, mission: string, file: File) =>
    json<{ hash: string; name: string }>(
      "POST",
      `/teams/${teamId}/missions/${encodeURIComponent(mission)}/files?${new URLSearchParams({ name: file.name }).toString()}`,
      file,
    ),
  removeMissionFile: (teamId: string, mission: string, hash: string) =>
    json<{ removed: boolean }>("DELETE", `/teams/${teamId}/missions/${encodeURIComponent(mission)}/files/${hash}`),
  missionFileContent: (teamId: string, mission: string, hash: string) =>
    download(`/teams/${teamId}/missions/${encodeURIComponent(mission)}/files/${hash}/content`, "file"),

  // Maps of a team's channel and of the events it takes part in.
  teamMaps: (teamId: string) => json<TeamMaps>("GET", `/teams/${teamId}/maps`),
  setTeamMaps: (teamId: string, sourceIds: string[]) => json<Synced>("PUT", `/teams/${teamId}/maps`, { source_ids: sourceIds }),
  /** A data package of the maps the team's devices get now, to import by hand. */
  teamMapsPackage: (teamId: string) => download(`/teams/${teamId}/maps/package`, "maps.zip"),
  /** A leader adds map sources of the team's own: one file, a map source or a zip of them, in base64. */
  uploadTeamMapSource: (teamId: string, name: string, data: string) =>
    json<Synced & { results: MapUploadResult[] }>("POST", `/teams/${teamId}/map-sources`, { name, data }),
  removeTeamMapSource: (teamId: string, sourceId: string) => json<Synced>("DELETE", `/teams/${teamId}/map-sources/${sourceId}`),
  /** Checks a map source again now; answers it as it is afterwards. */
  checkMapSource: (id: string) => json<MapSource>("POST", `/map-sources/${id}/check`),
  eventMaps: (teamId: string, eventId: string) => json<ChannelMaps>("GET", `/teams/${teamId}/events/${eventId}/maps`),
  setEventMaps: (teamId: string, eventId: string, sourceIds: string[]) =>
    json<Synced>("PUT", `/teams/${teamId}/events/${eventId}/maps`, { source_ids: sourceIds }),

  adminTeams: () => json<AdminTeams>("GET", "/admin/teams"),
  adminEvents: () => json<{ events: TeamEvent[] }>("GET", "/admin/events"),
  adminCancelEvent: (eventId: string) => json<Synced>("POST", `/admin/events/${eventId}/cancel`),
  // Events the administrators organise: no host team, the teams are put in directly.
  adminCreateEvent: (body: { name: string; starts_at?: string; ends_at: string; team_ids: string[] }) =>
    json<Synced & { id: string; group_name: string }>("POST", "/admin/events", body),
  adminUpdateEvent: (eventId: string, dates: { starts_at?: string; ends_at?: string }) =>
    json<Synced>("PATCH", `/admin/events/${eventId}`, dates),
  adminEndEvent: (eventId: string) => json<Synced>("POST", `/admin/events/${eventId}/end`),
  adminAssignTeam: (eventId: string, teamId: string) =>
    json<Synced & { team_id: string; name: string }>("POST", `/admin/events/${eventId}/teams`, { team_id: teamId }),
  adminRemoveTeamFromEvent: (eventId: string, teamId: string) =>
    json<Synced>("DELETE", `/admin/events/${eventId}/teams/${teamId}`),
  adminEventMaps: (eventId: string) => json<ChannelMaps>("GET", `/admin/events/${eventId}/maps`),
  adminSetEventMaps: (eventId: string, sourceIds: string[]) =>
    json<Synced>("PUT", `/admin/events/${eventId}/maps`, { source_ids: sourceIds }),
  // The map catalogue (administrators).
  mapCatalogue: () => json<MapCatalogue>("GET", "/admin/map-sources"),
  /** One file, a map source or a zip of them; `data` is its content in base64. */
  uploadMapSource: (name: string, data: string) =>
    json<Synced & { results: MapUploadResult[] }>("POST", "/admin/map-sources", { name, data }),
  addMapSuggestions: (keys: string[]) => json<{ results: MapUploadResult[] }>("POST", "/admin/map-sources/suggestions", { keys }),
  removeMapSource: (id: string) => json<Synced>("DELETE", `/admin/map-sources/${id}`),
  decideTeam: (id: string, approve: boolean, options: { member_cap?: number; note?: string }) =>
    json<Team>("POST", `/admin/teams/${id}/decision`, { approve, ...options }),
  adminUpdateTeam: (id: string, changes: { member_cap?: number; suspended?: boolean }) =>
    json<Synced & { team: AdminTeam }>("PATCH", `/admin/teams/${id}`, changes),
  adminAccounts: () => json<{ accounts: AdminAccount[] }>("GET", "/admin/accounts"),
  setAdmin: (id: string, isAdmin: boolean) => json<void>("PATCH", `/admin/accounts/${id}`, { is_admin: isAdmin }),
  /** Deletes the account and what is tied to it, and e-mails the address what was deleted. */
  deleteAccount: (id: string) =>
    json<Synced & { removed_accesses: number; removed_streams: number; withdrawn_teams: number; email_sent: boolean }>(
      "DELETE",
      `/admin/accounts/${id}`,
    ),
  /** Creates an account for the address and e-mails it an invitation, in `language`. */
  inviteAccount: (email: string, language: string) =>
    json<{ id: string; email: string; email_sent: boolean }>("POST", "/admin/accounts", { email, language }),
  reconciliation: () => json<Reconciliation>("GET", "/admin/reconciliation"),
  runReconciliation: () => json<Reconciliation>("POST", "/admin/reconciliation/run"),

  // Monitoring (administrators). What is shown is what the platform last saw; `refresh`
  // makes it look first.
  monitoring: (refresh = false) => json<Monitoring>("GET", `/admin/monitoring${refresh ? "?refresh=true" : ""}`),
  monitoringHistory: (range: HistoryRange) =>
    json<History>(
      "GET",
      `/admin/monitoring/history?${"hours" in range ? `hours=${range.hours}` : `from=${Math.floor(range.from / 1000)}&to=${Math.ceil(range.to / 1000)}`}`,
    ),
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
