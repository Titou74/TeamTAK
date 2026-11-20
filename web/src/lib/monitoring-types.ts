// Shapes of the monitoring answers (crates/server/src/monitor, crates/agent/src/report.rs).

export type Severity = "low" | "warning" | "critical";

export interface Alert {
  id: number;
  /** Stable code, translated with `alerts.kinds.<kind>`. */
  kind: string;
  /** What it is about when a kind can have several at once: a service, a filesystem. */
  subject: string;
  severity: Severity;
  /** Figures the sentence is filled with. */
  details: Record<string, string | number | boolean>;
  raised_at: string;
  last_seen_at: string;
  resolved_at: string | null;
}

export interface AlertsSummary {
  open: number;
  critical: number;
  warning: number;
  low: number;
}

export interface Disk {
  name: string;
  also: string[];
  total_bytes: number;
  available_bytes: number;
  used_percent: number;
}

export interface Host {
  os: { id: string; id_like: string[]; version_id: string; pretty_name: string };
  kernel: string | null;
  uptime_seconds: number | null;
  cpu: { cores: number | null; percent: number | null; load1: number | null; load5: number | null; load15: number | null };
  memory: { total_bytes: number; available_bytes: number; swap_total_bytes: number; swap_free_bytes: number } | null;
  disks: Disk[];
  clock: { synchronized: boolean | null; max_error_ms: number | null };
  updates: {
    manager: string | null;
    pending: number | null;
    security: number | null;
    lists_age_seconds: number | null;
    reboot_required: boolean | null;
    reboot_packages: string[];
    error: string | null;
  };
}

export interface ImageUpdate {
  source: "local" | "tag" | "digest" | "unknown";
  status: "local" | "up_to_date" | "update_available" | "pinned" | "pinned_newer_exists" | "unknown";
  checked_at: string | null;
  error: string | null;
}

export interface Service {
  service: string;
  present: boolean;
  state: string | null;
  health: string | null;
  restart_count: number;
  recent_restarts: number;
  started_at: string | null;
  image: string;
  image_created: string | null;
  restartable: boolean;
  update: ImageUpdate;
}

export interface Certificate {
  id: string;
  subject: string;
  issuer: string;
  expires_at: string;
  days_left: number;
  level: "ok" | "warning" | "critical" | "expired";
  from: "file" | "presented";
  stale: boolean;
}

export interface ClientCertificate {
  username: string;
  team_id: string;
  team_name: string;
  source: "package" | "device";
  expires_at: string;
  days_left: number;
}

export interface BackupFile {
  name: string;
  size_bytes: number;
  modified_at: string | null;
  encrypted: boolean;
}

export interface BackupRun {
  id: number;
  started_at: string;
  finished_at: string | null;
  trigger: "schedule" | "manual";
  requested_by: string;
  ok: boolean | null;
  file: string | null;
  size_bytes: number | null;
  encrypted: boolean;
  error: string | null;
}

export interface Backups {
  enabled: boolean;
  hour_utc: number;
  keep: number;
  passphrase_set: boolean;
  max_age_hours: number;
  files: BackupFile[];
  total_bytes: number;
  running: boolean;
  error: string | null;
  newest_at: string | null;
  runs: BackupRun[];
  daily_ran_at: string | null;
  daily_report: {
    backup?: { ok?: boolean; skipped?: string; error?: string | null };
    letsencrypt?: { outcome: string };
    logs?: { deleted_files?: number; freed_bytes?: number; rotated_files?: number; remaining_bytes?: number; error?: string };
    purged?: Record<string, number>;
  };
}

export interface Snapshot {
  collected_at: string;
  interval_seconds: number;
  agent: { status: "ok" | "unreachable" | "disabled"; error: string | null; version: string | null };
  mode: string;
  host: Host | null;
  services: { services: Service[]; error: string | null };
  tak: {
    ports: { port: number; role: "clients" | "api" | "enrollment"; open: boolean }[];
    api_ok: boolean;
    version: string | null;
    installed_version: string;
    zip_versions: string[];
    other_zip_versions: string[];
    latest_known_version: string;
    newer_known: boolean;
    processes: { name: string; running: boolean }[];
    processes_error: string | null;
    logs_bytes: number | null;
  };
  certificates: Certificate[];
  client_certificates: {
    active: number;
    expiring_warning: number;
    expiring_critical: number;
    warning_days: number;
    critical_days: number;
    soonest: ClientCertificate[];
  };
  platform: {
    reconcile: {
      last_run_at: string | null;
      last_ok_at: string | null;
      desired_at: string | null;
      directory_reachable: boolean;
      tak_reachable: boolean;
      errors: string[];
      interval_seconds: number;
    };
    drift_24h: number;
    drift_7d: number;
    overdue_events: { group: string; name: string; boundary: "start" | "end"; due_at: string }[];
    mail: {
      configured: boolean;
      failures_last_hour: number;
      failures_24h: number;
      last_failure_at: string | null;
      last_success_at: string | null;
      last_error: string | null;
    };
    directory_open: boolean;
    started_at: string | null;
    release: Release;
  };
  backups: Backups;
  letsencrypt: { enabled: boolean; certificate_found: boolean; handed_over: boolean } | null;
  devices: { identified: number; unidentified: number };
}

/** TeamTAK's own version, and the latest release published on GitHub. */
export interface Release {
  installed: string;
  /** owner/name on GitHub; null when the check is turned off (RELEASE_REPOSITORY=off). */
  repository: string | null;
  /** unknown: GitHub has not answered yet; no_release: none published yet. */
  status: "up_to_date" | "update_available" | "no_release" | "unknown" | "off";
  latest: { version: string; tag: string; name: string; url: string; published_at: string | null } | null;
  checked_at: string | null;
  error: string | null;
}

export interface Monitoring {
  /** Null during the few seconds between a start of the platform and its first look. */
  snapshot: Snapshot | null;
  alerts: { open: Alert[]; resolved: Alert[] };
  notifications: {
    emails: boolean;
    webhook_set: boolean;
    webhook_last: { at: string; ok: boolean; error: string | null } | null;
    reminder_hours: number;
  };
}

export interface Sample {
  sampled_at: string;
  cpu_percent: number | null;
  load1: number | null;
  memory_percent: number | null;
  swap_percent: number | null;
  disk_percent: number | null;
  devices: number | null;
}

export interface History {
  hours: number;
  retention_days: number;
  samples: Sample[];
}

export interface Device {
  id: string;
  /** Empty for a session without any group, which TAK Server lists without its account. */
  username: string;
  callsign: string | null;
  client: string | null;
  groups: string[];
  connected_since: string;
  /** The device was already connected when the platform started watching. */
  since_is_lower_bound: boolean;
  last_report_at: string | null;
  silent: boolean;
  access: { kind: "shared" | "individual" | "external"; label: string; owner: string | null; team_id: string; team_name: string } | null;
  /** Administrators only. */
  ip_address?: string | null;
}

export interface Devices {
  /** When TAK Server last answered; null when it never did since the platform started. */
  as_of: string | null;
  devices: Device[];
  unidentified?: Device[];
}

export interface Thresholds {
  disk_percent: number;
  memory_percent: number;
  certificate_warning_days: number;
  certificate_critical_days: number;
  backup_max_age_hours: number;
  mail_failures_per_hour: number;
  restart_loop_count: number;
  reconcile_stale_minutes: number;
}

export interface Settings {
  thresholds: Thresholds;
  alert_emails: boolean;
  webhook_url: string;
  reminder_hours: number;
  backup_enabled: boolean;
  backup_hour_utc: number;
  backup_keep: number;
  audit_retention_days: number;
  log_keep_days: number;
  tak_latest_known_version: string;
}

export interface SettingsView {
  settings: Settings;
  defaults: Settings;
  backup_passphrase_set: boolean;
}

export interface AuditEntry {
  id: number;
  occurred_at: string;
  /** Empty for what the platform does by itself and for the server's command line. */
  actor_email: string;
  action: string;
  team_id: string | null;
  team_name: string;
  object: string;
  details: Record<string, string | number | boolean>;
  ip: string;
}

export interface AuditPage {
  entries: AuditEntry[];
  total: number;
  page: number;
  page_size: number;
  actions: string[];
  teams: { id: string; name: string }[];
  retention_days: number;
}

export interface AuditFilter {
  from?: string;
  to?: string;
  team_id?: string;
  actor?: string;
  action?: string;
  page?: number;
}
