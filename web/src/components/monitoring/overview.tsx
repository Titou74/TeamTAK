import { useQueryClient } from "@tanstack/react-query";
import { BellIcon, CircleCheckIcon, RotateCwIcon, TriangleAlertIcon } from "lucide-react";
import { type ReactNode, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router-dom";
import { toast } from "sonner";

import { ConfirmDialog } from "@/components/confirm-dialog";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { api } from "@/lib/api";
import {
  formatBytes,
  formatDate,
  formatDateTime,
  formatDateTimeRange,
  formatDuration,
  formatList,
  formatNumber,
  formatPercent,
  formatRelative,
} from "@/lib/format";
import { useNotifyError } from "@/lib/hooks";
import type { Alert as AlertData, Certificate, Monitoring, Release, Service, Snapshot } from "@/lib/monitoring-types";

import { HistoryCharts } from "./charts";
import { Command, Field, Meter, ResolvedBadge, SeverityBadge, StateDot, useAlertText } from "./shared";

/** Thresholds used to colour the gauges; the alerts themselves follow the settings. */
const DISK_WARN = 90;
const MEMORY_WARN = 92;

// Commands shown to the administrator, to run over SSH. The platform never runs them.
const APT_UPGRADE = "sudo apt update && sudo apt full-upgrade";
const REBOOT = "sudo reboot";
const PULL_IMAGES = "cd deploy && docker compose pull && docker compose up -d";
const UPGRADE_TAK = "sudo ./install.sh --upgrade-tak";
const UPDATE_TEAMTAK = "sudo deploy/scripts/backup.sh && git pull && sudo ./install.sh";
const START_AGENT = "cd deploy && docker compose up -d agent && docker compose logs --tail 20 agent";

/** An image reference with its digest shortened to what the eye can compare. */
function shortImage(image: string): string {
  return image.replace(/@sha256:([0-9a-f]{12})[0-9a-f]+$/, "@sha256:$1…");
}

/** An alert resolved this recently is shown without having to unfold the list. */
const RECENTLY_RESOLVED_MS = 24 * 3600 * 1000;

function AlertsCard({ data }: { data: Monitoring }) {
  const { t } = useTranslation();
  const alertText = useAlertText();
  const { open, resolved } = data.alerts;
  const openRow = (alert: AlertData) => (
    <li key={alert.id} className="flex flex-wrap items-baseline gap-x-2 gap-y-0.5 py-1.5">
      <SeverityBadge severity={alert.severity} />
      <span className="font-medium">{alertText(alert)}</span>
      <span className="text-xs text-muted-foreground" title={formatDateTime(alert.raised_at)}>
        {t("alerts.since", { when: formatRelative(alert.raised_at) })}
      </span>
    </li>
  );
  // A resolved alert says so first, in green: its sentence describes the problem as it was.
  const resolvedRow = (alert: AlertData) => {
    const resolvedAt = alert.resolved_at ?? alert.last_seen_at;
    return (
      <li key={alert.id} className="flex flex-wrap items-baseline gap-x-2 gap-y-0.5 py-1.5">
        <ResolvedBadge />
        <span className="text-muted-foreground">{alertText(alert)}</span>
        <span className="text-xs text-muted-foreground" title={formatDateTimeRange(alert.raised_at, resolvedAt)}>
          {t("alerts.resolvedAfter", { when: formatRelative(resolvedAt), duration: formatDuration(alert.raised_at, resolvedAt) })}
        </span>
      </li>
    );
  };
  // Measured from the platform's last look, which the page refreshes.
  const lookedAt = data.snapshot ? new Date(data.snapshot.collected_at).getTime() : null;
  const recentlyResolved =
    lookedAt !== null && resolved.some((alert) => alert.resolved_at && lookedAt - new Date(alert.resolved_at).getTime() < RECENTLY_RESOLVED_MS);
  const channels = [
    data.notifications.emails ? t("alerts.channels.email") : t("alerts.channels.emailOff"),
    ...(data.notifications.webhook_set ? [data.notifications.webhook_last?.ok === false ? t("alerts.channels.webhookFailing") : t("alerts.channels.webhook")] : []),
  ];
  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          {open.length === 0 ? <CircleCheckIcon className="size-4 text-emerald-600" /> : <BellIcon className="size-4 text-destructive" />}
          {t("alerts.title", { count: open.length })}
        </CardTitle>
        <CardDescription>
          {formatList(channels)}. {t("alerts.reminder", { count: data.notifications.reminder_hours })}
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-3">
        {open.length === 0 ? <p className="text-sm text-muted-foreground">{t("alerts.none")}</p> : <ul className="divide-y text-sm">{open.map(openRow)}</ul>}
        {resolved.length > 0 && (
          <details className="text-sm" open={recentlyResolved}>
            <summary className="cursor-pointer text-muted-foreground">{t("alerts.resolvedTitle", { count: resolved.length })}</summary>
            <ul className="mt-1 divide-y">{resolved.map(resolvedRow)}</ul>
          </details>
        )}
      </CardContent>
    </Card>
  );
}

function HostCard({ snapshot }: { snapshot: Snapshot }) {
  const { t, i18n } = useTranslation();
  const host = snapshot.host;
  if (!host) return null;
  const memory = host.memory;
  const memoryUsed = memory ? ((memory.total_bytes - memory.available_bytes) / memory.total_bytes) * 100 : null;
  const swapUsed = memory && memory.swap_total_bytes > 0 ? ((memory.swap_total_bytes - memory.swap_free_bytes) / memory.swap_total_bytes) * 100 : null;
  const updates = host.updates;
  // Durations the agent measured, turned into dates from the moment it measured them.
  const measuredAt = new Date(snapshot.collected_at).getTime();
  const started = host.uptime_seconds === null ? null : new Date(measuredAt - host.uptime_seconds * 1000).toISOString();
  const listsAt = updates.lists_age_seconds === null ? null : new Date(measuredAt - updates.lists_age_seconds * 1000).toISOString();
  const diskName = (name: string) => (i18n.exists(`monitoring.host.disk.${name}`) ? t(`monitoring.host.disk.${name}`) : name);
  return (
    <Card>
      <CardHeader>
        <CardTitle>{t("monitoring.host.title")}</CardTitle>
        <CardDescription translate="no">{[host.os.pretty_name, host.kernel].filter(Boolean).join(" · ")}</CardDescription>
      </CardHeader>
      <CardContent className="space-y-4 text-sm">
        <dl className="grid gap-x-8 sm:grid-cols-2">
          <Field label={t("monitoring.host.cpu")}>
            {host.cpu.percent === null ? t("monitoring.unknown") : formatPercent(host.cpu.percent)}
            {host.cpu.cores !== null && <span className="text-muted-foreground"> · {t("monitoring.host.cores", { count: host.cpu.cores })}</span>}
          </Field>
          <Field label={t("monitoring.host.load")}>
            <span className="tabular-nums">{[host.cpu.load1, host.cpu.load5, host.cpu.load15].map((v) => (v === null ? "–" : formatNumber(v))).join(" · ")}</span>
          </Field>
          <Field label={t("monitoring.host.memory")}>
            {memory && memoryUsed !== null ? (
              <>
                {formatPercent(memoryUsed)}
                <span className="text-muted-foreground"> · {t("monitoring.host.freeOf", { free: formatBytes(memory.available_bytes), total: formatBytes(memory.total_bytes) })}</span>
              </>
            ) : (
              t("monitoring.unknown")
            )}
          </Field>
          <Field label={t("monitoring.host.swap")}>
            {swapUsed === null ? <span className="text-muted-foreground">{t("monitoring.host.noSwap")}</span> : formatPercent(swapUsed)}
          </Field>
          <Field label={t("monitoring.host.started")}>{started ? formatRelative(started) : t("monitoring.unknown")}</Field>
          <Field label={t("monitoring.host.clock")}>
            {host.clock.synchronized === null ? (
              t("monitoring.unknown")
            ) : (
              <span className="inline-flex items-center gap-1.5">
                <StateDot state={host.clock.synchronized ? "ok" : "warning"} />
                {host.clock.synchronized ? t("monitoring.host.clockOk") : t("monitoring.host.clockOff")}
              </span>
            )}
          </Field>
        </dl>
        {memory && memoryUsed !== null && <Meter percent={memoryUsed} warnAt={MEMORY_WARN} label={t("monitoring.host.memory")} />}

        <div className="space-y-2">
          <h3 className="font-medium">{t("monitoring.host.disks")}</h3>
          {host.disks.map((disk) => (
            <div key={disk.name} className="space-y-1">
              <div className="flex flex-wrap items-baseline justify-between gap-2">
                <span>
                  {diskName(disk.name)}
                  {disk.also.length > 0 && <span className="text-muted-foreground"> · {t("monitoring.host.alsoHolds", { list: formatList(disk.also.map(diskName)) })}</span>}
                </span>
                <span className="tabular-nums">
                  {formatPercent(disk.used_percent)}
                  <span className="text-muted-foreground"> · {t("monitoring.host.freeOf", { free: formatBytes(disk.available_bytes), total: formatBytes(disk.total_bytes) })}</span>
                </span>
              </div>
              <Meter percent={disk.used_percent} warnAt={DISK_WARN} label={diskName(disk.name)} />
            </div>
          ))}
        </div>

        <div className="space-y-2">
          <h3 className="font-medium">{t("monitoring.updates.title")}</h3>
          {updates.manager === null ? (
            <p className="text-muted-foreground">{t("monitoring.updates.unknownSystem")}</p>
          ) : (
            <>
              <p className="flex flex-wrap items-center gap-x-3 gap-y-1">
                {updates.pending === null ? (
                  <span className="text-muted-foreground">{t("monitoring.updates.notRead")}</span>
                ) : updates.pending === 0 ? (
                  <span className="inline-flex items-center gap-1.5">
                    <StateDot state="ok" />
                    {t("monitoring.updates.upToDate")}
                  </span>
                ) : (
                  <span className="inline-flex items-center gap-1.5">
                    <StateDot state={updates.security ? "warning" : "unknown"} />
                    {t("monitoring.updates.pending", { count: updates.pending })}
                    {updates.security !== null && updates.security > 0 && <Badge variant="destructive">{t("monitoring.updates.security", { count: updates.security })}</Badge>}
                  </span>
                )}
                {updates.reboot_required === true && <Badge variant="destructive">{t("monitoring.updates.rebootRequired")}</Badge>}
                {updates.reboot_required === false && <span className="text-muted-foreground">{t("monitoring.updates.noReboot")}</span>}
              </p>
              {listsAt && (
                <p className="text-xs text-muted-foreground">
                  {t("monitoring.updates.listsAge", { when: formatRelative(listsAt) })}
                  {(updates.lists_age_seconds ?? 0) > 7 * 86400 && <> {t("monitoring.updates.listsStale")}</>}
                </p>
              )}
              {updates.error && (
                <p className="font-mono text-xs text-destructive" lang="en">
                  {updates.error}
                </p>
              )}
              {((updates.pending ?? 0) > 0 || updates.reboot_required) && (
                <div className="space-y-1.5">
                  <p className="text-xs text-muted-foreground">{t("monitoring.updates.howTo")}</p>
                  {(updates.pending ?? 0) > 0 && <Command label={t("monitoring.updates.commandLabel")}>{APT_UPGRADE}</Command>}
                  {updates.reboot_required && <Command label={t("monitoring.updates.rebootLabel")}>{REBOOT}</Command>}
                </div>
              )}
            </>
          )}
        </div>
      </CardContent>
    </Card>
  );
}

function serviceState(service: Service): "ok" | "warning" | "bad" | "unknown" {
  if (!service.present) return "bad";
  if (service.state !== "running") return "bad";
  if (service.health === "unhealthy") return "warning";
  if (service.health === "starting") return "unknown";
  return "ok";
}

/** What a service uses now: processor, memory, and the space its data takes. */
function ServiceUsage({ service }: { service: Service }) {
  const { t } = useTranslation();
  const figures = [
    { label: t("monitoring.services.usage.cpu"), value: service.cpu_percent === null ? null : formatPercent(service.cpu_percent, service.cpu_percent < 10 ? 1 : 0) },
    { label: t("monitoring.services.usage.memory"), value: service.memory_bytes === null ? null : formatBytes(service.memory_bytes) },
    { label: t("monitoring.services.usage.disk"), value: service.disk_bytes === null ? null : formatBytes(service.disk_bytes) },
  ].filter((f) => f.value !== null);
  if (figures.length === 0) return <span className="text-muted-foreground">—</span>;
  return (
    <dl className="grid grid-cols-[auto_auto] gap-x-2 text-xs">
      {figures.map((f) => (
        <div key={f.label} className="contents">
          <dt className="text-muted-foreground">{f.label}</dt>
          <dd className="text-right tabular-nums">{f.value}</dd>
        </div>
      ))}
    </dl>
  );
}

function ServicesCard({ snapshot, reload }: { snapshot: Snapshot; reload: () => Promise<unknown> }) {
  const { t, i18n } = useTranslation();
  const notifyError = useNotifyError();
  const [restarting, setRestarting] = useState<Service | null>(null);
  const services = snapshot.services.services;
  if (snapshot.agent.status !== "ok") return null;

  async function restart(service: Service) {
    try {
      const result = await api.restartService(service.service);
      if (result.restarted) toast.success(t("monitoring.services.restarted", { service: service.service }));
      else toast.info(t("monitoring.services.restartAccepted", { service: service.service }));
      await reload();
    } catch (e) {
      notifyError(e);
    }
  }

  const stateLabel = (service: Service) => {
    if (!service.present) return t("monitoring.services.state.missing");
    const state = i18n.exists(`monitoring.services.state.${service.state}`) ? t(`monitoring.services.state.${service.state}`) : (service.state ?? "");
    return service.health && service.state === "running" ? `${state} · ${t(`monitoring.services.health.${service.health}`)}` : state;
  };
  const restartWarning = (service: string) =>
    i18n.exists(`monitoring.services.restartText.${service}`) ? t(`monitoring.services.restartText.${service}`) : t("monitoring.services.restartText.other");
  const anyUpdate = services.some((s) => s.update.status === "update_available");
  return (
    <Card>
      <CardHeader>
        <CardTitle>{t("monitoring.services.title")}</CardTitle>
        <CardDescription>{t("monitoring.services.description")}</CardDescription>
      </CardHeader>
      <CardContent className="space-y-3">
        {snapshot.services.error && (
          <Alert variant="destructive">
            <AlertTitle>{t("monitoring.services.dockerError")}</AlertTitle>
            <AlertDescription className="font-mono text-xs" lang="en">
              {snapshot.services.error}
            </AlertDescription>
          </Alert>
        )}
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>{t("monitoring.services.columns.service")}</TableHead>
              <TableHead>{t("monitoring.services.columns.state")}</TableHead>
              <TableHead>{t("monitoring.services.columns.usage")}</TableHead>
              <TableHead>{t("monitoring.services.columns.image")}</TableHead>
              <TableHead />
            </TableRow>
          </TableHeader>
          <TableBody>
            {services.map((service) => (
              <TableRow key={service.service}>
                <TableCell className="font-medium" translate="no">
                  {service.service}
                </TableCell>
                <TableCell>
                  <span className="inline-flex items-center gap-1.5">
                    <StateDot state={serviceState(service)} />
                    {stateLabel(service)}
                  </span>
                  <div className="text-xs text-muted-foreground">
                    {service.started_at && service.state === "running" && t("monitoring.services.since", { when: formatRelative(service.started_at) })}
                    {service.restart_count > 0 && (
                      <>
                        {" · "}
                        <span className={service.recent_restarts > 0 ? "text-destructive" : ""}>
                          {t("monitoring.services.restarts", { count: service.restart_count })}
                          {service.recent_restarts > 0 && <> {t("monitoring.services.recentRestarts", { count: service.recent_restarts })}</>}
                        </span>
                      </>
                    )}
                  </div>
                </TableCell>
                <TableCell>
                  <ServiceUsage service={service} />
                </TableCell>
                <TableCell className="whitespace-normal">
                  <span className="font-mono text-xs break-all" translate="no">
                    {shortImage(service.image)}
                  </span>
                  {service.present && (
                    <div className="mt-0.5 flex flex-wrap items-center gap-1.5 text-xs text-muted-foreground">
                      <Badge variant={service.update.status === "update_available" ? "destructive" : "outline"}>{t(`monitoring.services.update.${service.update.status}`)}</Badge>
                      {service.image_created && t("monitoring.services.built", { date: formatDate(service.image_created) })}
                      {service.update.checked_at && <> · {t("monitoring.services.checked", { when: formatRelative(service.update.checked_at) })}</>}
                    </div>
                  )}
                </TableCell>
                <TableCell className="text-right">
                  {service.restartable && service.present && (
                    <Button variant="ghost" size="sm" onClick={() => setRestarting(service)}>
                      <RotateCwIcon />
                      {t("monitoring.services.restart")}
                    </Button>
                  )}
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
        <details className="text-sm">
          <summary className="cursor-pointer text-muted-foreground">{t("monitoring.services.updateHelpTitle")}</summary>
          <div className="mt-2 space-y-2 text-muted-foreground">
            <p>{t("monitoring.services.updateHelp")}</p>
            <Command label={t("monitoring.services.updateCommandLabel")}>{PULL_IMAGES}</Command>
          </div>
        </details>
        {anyUpdate && <p className="text-sm">{t("monitoring.services.updateAvailable")}</p>}
      </CardContent>
      <ConfirmDialog
        open={restarting !== null}
        onOpenChange={(open) => !open && setRestarting(null)}
        title={t("monitoring.services.restartTitle", { service: restarting?.service ?? "" })}
        description={<p>{restartWarning(restarting?.service ?? "")}</p>}
        confirmLabel={t("monitoring.services.restart")}
        destructive
        onConfirm={() => (restarting ? restart(restarting) : Promise.resolve())}
      />
    </Card>
  );
}

function TakCard({ snapshot }: { snapshot: Snapshot }) {
  const { t, i18n } = useTranslation();
  const tak = snapshot.tak;
  const devices = snapshot.devices;
  return (
    <Card>
      <CardHeader>
        <CardTitle>{t("monitoring.tak.title")}</CardTitle>
        <CardDescription>
          {tak.version ? t("monitoring.tak.running", { version: tak.version }) : t("monitoring.tak.notAnswering")}
          {tak.api_ok && <> · {t("monitoring.tak.devices", { count: devices.identified })}</>}
          {tak.api_ok && devices.unidentified > 0 && <> · {t("monitoring.tak.unidentified", { count: devices.unidentified })}</>}
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4 text-sm">
        <div className="grid gap-2 sm:grid-cols-3">
          {tak.ports.map((port) => (
            <div key={port.port} className="flex items-center gap-2 rounded-lg border bg-card p-2.5">
              <StateDot state={port.open ? "ok" : "bad"} />
              <span className="flex-1">
                <span className="font-medium tabular-nums">{port.port}</span> <span className="text-muted-foreground">{t(`monitoring.tak.port.${port.role}`)}</span>
              </span>
              <span className={port.open ? "text-muted-foreground" : "text-destructive"}>{port.open ? t("monitoring.tak.open") : t("monitoring.tak.closed")}</span>
            </div>
          ))}
        </div>
        {tak.processes.length > 0 && (
          <div className="space-y-1.5">
            <h3 className="font-medium">{t("monitoring.tak.processes")}</h3>
            <ul className="flex flex-wrap gap-x-4 gap-y-1">
              {tak.processes.map((process) => (
                <li key={process.name} className="inline-flex items-center gap-1.5">
                  <StateDot state={process.running ? "ok" : "bad"} />
                  {i18n.exists(`monitoring.tak.process.${process.name}`) ? t(`monitoring.tak.process.${process.name}`) : process.name}
                </li>
              ))}
            </ul>
          </div>
        )}
        {tak.processes_error && snapshot.agent.status === "ok" && <p className="text-muted-foreground">{t("monitoring.tak.processesUnknown")}</p>}

        <div className="space-y-2">
          <h3 className="font-medium">{t("monitoring.tak.versionTitle")}</h3>
          <dl>
            <Field label={t("monitoring.tak.installed")}>
              <span translate="no">{tak.installed_version || tak.version || t("monitoring.unknown")}</span>
            </Field>
            <Field label={t("monitoring.tak.latestKnown")}>
              <span translate="no">{tak.latest_known_version || t("monitoring.tak.notRecorded")}</span>
            </Field>
            {tak.logs_bytes !== null && <Field label={t("monitoring.tak.logs")}>{formatBytes(tak.logs_bytes)}</Field>}
          </dl>
          {tak.newer_known && (
            <Alert>
              <TriangleAlertIcon />
              <AlertTitle>{t("monitoring.tak.newerTitle", { version: tak.latest_known_version })}</AlertTitle>
              <AlertDescription>{t("monitoring.tak.newerText")}</AlertDescription>
            </Alert>
          )}
          {tak.other_zip_versions.length > 0 && (
            <Alert>
              <TriangleAlertIcon />
              <AlertTitle>{t("monitoring.tak.zipTitle", { version: formatList(tak.other_zip_versions) })}</AlertTitle>
              <AlertDescription className="space-y-2">
                <p>{t("monitoring.tak.zipText")}</p>
                <Command label={t("monitoring.tak.upgradeLabel")}>{UPGRADE_TAK}</Command>
              </AlertDescription>
            </Alert>
          )}
          <p className="text-xs text-muted-foreground">{t("monitoring.tak.versionHelp")}</p>
        </div>
      </CardContent>
    </Card>
  );
}

function ExpiryCell({ daysLeft, level }: { daysLeft: number; level: Certificate["level"] }) {
  const { t } = useTranslation();
  if (level === "expired") return <Badge variant="destructive">{t("monitoring.certs.expired")}</Badge>;
  const text = t("monitoring.certs.daysLeft", { count: daysLeft });
  if (level === "critical") return <Badge variant="destructive">{text}</Badge>;
  if (level === "warning") return <Badge>{text}</Badge>;
  return <span className="text-muted-foreground">{text}</span>;
}

function CertificatesCard({ snapshot }: { snapshot: Snapshot }) {
  const { t } = useTranslation();
  const clients = snapshot.client_certificates;
  const level = (days: number): Certificate["level"] => (days < 0 ? "expired" : days <= clients.critical_days ? "critical" : days <= clients.warning_days ? "warning" : "ok");
  return (
    <Card>
      <CardHeader>
        <CardTitle>{t("monitoring.certs.title")}</CardTitle>
        <CardDescription>{t("monitoring.certs.description", { warning: clients.warning_days, critical: clients.critical_days })}</CardDescription>
      </CardHeader>
      <CardContent className="space-y-5">
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>{t("monitoring.certs.columns.certificate")}</TableHead>
              <TableHead>{t("monitoring.certs.columns.expires")}</TableHead>
              <TableHead />
            </TableRow>
          </TableHeader>
          <TableBody>
            {snapshot.certificates.map((certificate) => (
              <TableRow key={certificate.id}>
                <TableCell className="whitespace-normal">
                  <span className="font-medium">{t(`monitoring.certs.name.${certificate.id}`)}</span>
                  <div className="text-xs text-muted-foreground" translate="no">
                    {certificate.subject}
                    {certificate.issuer && certificate.issuer !== certificate.subject && <span translate="yes"> · {t("monitoring.certs.issuedBy", { issuer: certificate.issuer })}</span>}
                    {certificate.stale && <span translate="yes"> · {t("monitoring.certs.stale")}</span>}
                  </div>
                </TableCell>
                <TableCell>{formatDate(certificate.expires_at)}</TableCell>
                <TableCell className="text-right">
                  <ExpiryCell daysLeft={certificate.days_left} level={certificate.level} />
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>

        <div className="space-y-2 text-sm">
          <h3 className="font-medium">{t("monitoring.certs.clientsTitle", { count: clients.active })}</h3>
          <p className="text-muted-foreground">
            {clients.expiring_warning === 0
              ? t("monitoring.certs.clientsNone", { days: clients.warning_days })
              : t("monitoring.certs.clientsSoon", { count: clients.expiring_warning, days: clients.warning_days, urgent: clients.expiring_critical, urgentDays: clients.critical_days })}{" "}
            {t("monitoring.certs.clientsHelp")}
          </p>
          {clients.soonest.length > 0 && (
            <details open={clients.expiring_warning > 0}>
              <summary className="cursor-pointer text-muted-foreground">{t("monitoring.certs.clientsNext")}</summary>
              <Table className="mt-1">
                <TableHeader>
                  <TableRow>
                    <TableHead>{t("monitoring.certs.columns.access")}</TableHead>
                    <TableHead>{t("monitoring.certs.columns.team")}</TableHead>
                    <TableHead>{t("monitoring.certs.columns.origin")}</TableHead>
                    <TableHead>{t("monitoring.certs.columns.expires")}</TableHead>
                    <TableHead />
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {clients.soonest.map((certificate, index) => (
                    <TableRow key={`${certificate.username}-${index}`}>
                      <TableCell className="font-mono text-xs" translate="no">
                        {certificate.username}
                      </TableCell>
                      <TableCell>
                        <Link to={`/teams/${certificate.team_id}`} className="hover:underline">
                          {certificate.team_name}
                        </Link>
                      </TableCell>
                      <TableCell className="text-muted-foreground">{t(`monitoring.certs.origin.${certificate.source}`)}</TableCell>
                      <TableCell>{formatDate(certificate.expires_at)}</TableCell>
                      <TableCell className="text-right">
                        <ExpiryCell daysLeft={certificate.days_left} level={level(certificate.days_left)} />
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </details>
          )}
        </div>
      </CardContent>
    </Card>
  );
}

/** The version that runs, the latest one on GitHub, and how to move to it. */
function ReleaseSection({ release, reload }: { release: Release; reload: () => Promise<unknown> }) {
  const { t } = useTranslation();
  const notifyError = useNotifyError();
  const [checking, setChecking] = useState(false);
  const latest = release.latest;

  async function checkAgain() {
    setChecking(true);
    try {
      const { release: checked } = await api.checkRelease();
      if (checked.error) toast.error(t("monitoring.release.checkFailed"));
      else if (checked.status === "update_available" && checked.latest) toast.info(t("monitoring.release.updateTitle", { version: checked.latest.version }));
      else toast.success(t("monitoring.release.noNewer"));
      await reload();
    } catch (e) {
      notifyError(e);
    } finally {
      setChecking(false);
    }
  }

  let latestText: ReactNode;
  if (release.status === "off") latestText = t("monitoring.release.off");
  else if (release.status === "unknown") latestText = t("monitoring.release.unknown");
  else if (release.status === "no_release" || !latest) latestText = t("monitoring.release.none");
  else
    latestText = (
      <span className="inline-flex items-center gap-1.5">
        <StateDot state={release.status === "update_available" ? "warning" : "ok"} />
        <a href={latest.url} target="_blank" rel="noreferrer" className="hover:underline" translate="no">
          {latest.version}
        </a>
        {latest.published_at && <span className="text-muted-foreground">({formatDate(latest.published_at)})</span>}
      </span>
    );
  return (
    <div className="space-y-2">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h3 className="font-medium">{t("monitoring.release.title")}</h3>
        {release.repository && (
          <Button variant="outline" size="sm" disabled={checking} onClick={() => void checkAgain()}>
            <RotateCwIcon className={checking ? "animate-spin" : undefined} />
            {checking ? t("common.working") : t("monitoring.release.checkAgain")}
          </Button>
        )}
      </div>
      <dl>
        <Field label={t("monitoring.release.installed")}>
          <span translate="no">{release.installed}</span>
        </Field>
        <Field label={t("monitoring.release.latest")}>{latestText}</Field>
        {release.checked_at && <Field label={t("monitoring.release.checked")}>{formatRelative(release.checked_at)}</Field>}
      </dl>
      {release.status === "update_available" && latest && (
        <Alert>
          <TriangleAlertIcon />
          <AlertTitle>{t("monitoring.release.updateTitle", { version: latest.version })}</AlertTitle>
          <AlertDescription className="space-y-2">
            <p>
              {t("monitoring.release.updateText")}{" "}
              <a href={latest.url} target="_blank" rel="noreferrer">
                {t("monitoring.release.notes")}
              </a>
            </p>
            <Command label={t("monitoring.release.commandLabel")}>{UPDATE_TEAMTAK}</Command>
          </AlertDescription>
        </Alert>
      )}
      {release.error && (
        <p className="font-mono text-xs text-muted-foreground" lang="en">
          {release.error}
        </p>
      )}
      {release.repository && <p className="text-xs text-muted-foreground">{t("monitoring.release.help", { repository: release.repository })}</p>}
    </div>
  );
}

function PlatformCard({ snapshot, reload }: { snapshot: Snapshot; reload: () => Promise<unknown> }) {
  const { t } = useTranslation();
  const notifyError = useNotifyError();
  const [syncing, setSyncing] = useState(false);
  const platform = snapshot.platform;
  const { reconcile, mail } = platform;
  const mailFailing = mail.last_error !== null;
  const le = snapshot.letsencrypt;

  async function handOver() {
    setSyncing(true);
    try {
      const result = await api.syncLetsEncrypt();
      toast.success(result.outcome === "updated" ? t("monitoring.letsencrypt.updated") : t("monitoring.letsencrypt.unchanged"));
      await reload();
    } catch (e) {
      notifyError(e);
    } finally {
      setSyncing(false);
    }
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t("monitoring.platform.title")}</CardTitle>
        <CardDescription>{t("monitoring.platform.description")}</CardDescription>
      </CardHeader>
      <CardContent className="space-y-4 text-sm">
        <dl className="grid gap-x-8 sm:grid-cols-2">
          <Field label={t("monitoring.platform.directory")}>
            <span className="inline-flex items-center gap-1.5">
              <StateDot state={platform.directory_open && reconcile.directory_reachable ? "ok" : "bad"} />
              {platform.directory_open && reconcile.directory_reachable ? t("admin.sync.reachable") : t("admin.sync.unreachable")}
            </span>
          </Field>
          <Field label={t("monitoring.platform.lastSync")}>{reconcile.last_ok_at ? formatRelative(reconcile.last_ok_at) : t("admin.accounts.never")}</Field>
          <Field label={t("monitoring.platform.drift")}>
            <span className={platform.drift_24h > 0 ? "font-medium text-destructive" : ""}>{t("monitoring.platform.driftValue", { day: platform.drift_24h, week: platform.drift_7d })}</span>
          </Field>
          <Field label={t("monitoring.platform.schedule")}>
            <span className="inline-flex items-center gap-1.5">
              <StateDot state={platform.overdue_events.length === 0 ? "ok" : "bad"} />
              {platform.overdue_events.length === 0 ? t("monitoring.platform.scheduleOk") : t("monitoring.platform.scheduleLate", { count: platform.overdue_events.length })}
            </span>
          </Field>
          <Field label={t("monitoring.platform.mail")}>
            <span className="inline-flex items-center gap-1.5">
              <StateDot state={!mail.configured || mailFailing ? "bad" : "ok"} />
              {!mail.configured ? t("monitoring.platform.mailOff") : mailFailing ? t("monitoring.platform.mailFailing") : t("monitoring.platform.mailOk")}
            </span>
          </Field>
          <Field label={t("monitoring.platform.mailFailures")}>{t("monitoring.platform.mailFailuresValue", { hour: mail.failures_last_hour, day: mail.failures_24h })}</Field>
        </dl>
        {platform.overdue_events.length > 0 && (
          <ul className="list-disc space-y-0.5 pl-5">
            {platform.overdue_events.map((event) => (
              <li key={`${event.group}-${event.boundary}`}>
                {event.boundary === "end"
                  ? t("monitoring.platform.eventNotEnded", { event: event.name, when: formatRelative(event.due_at) })
                  : t("monitoring.platform.eventNotStarted", { event: event.name, when: formatRelative(event.due_at) })}
              </li>
            ))}
          </ul>
        )}
        {mail.last_error && (
          <p className="font-mono text-xs text-destructive" lang="en">
            {mail.last_error}
          </p>
        )}
        {reconcile.errors.length > 0 && (
          <ul className="list-disc pl-5 font-mono text-xs text-destructive" lang="en">
            {reconcile.errors.map((error) => (
              <li key={error}>{error}</li>
            ))}
          </ul>
        )}
        {le?.enabled && (
          <div className="flex flex-wrap items-center justify-between gap-2 rounded-lg border p-3">
            <span className="inline-flex items-center gap-1.5">
              <StateDot state={le.handed_over ? "ok" : "warning"} />
              {!le.certificate_found ? t("monitoring.letsencrypt.noCertificate") : le.handed_over ? t("monitoring.letsencrypt.handedOver") : t("monitoring.letsencrypt.pending")}
            </span>
            <Button variant="outline" size="sm" disabled={syncing || !le.certificate_found} onClick={() => void handOver()}>
              {syncing ? t("common.working") : t("monitoring.letsencrypt.handOver")}
            </Button>
          </div>
        )}
        <ReleaseSection release={platform.release} reload={reload} />
      </CardContent>
    </Card>
  );
}

function AgentNotice({ snapshot }: { snapshot: Snapshot }) {
  const { t } = useTranslation();
  if (snapshot.agent.status === "ok") return null;
  const disabled = snapshot.agent.status === "disabled";
  return (
    <Alert variant="destructive">
      <TriangleAlertIcon />
      <AlertTitle>{disabled ? t("monitoring.agent.disabledTitle") : t("monitoring.agent.unreachableTitle")}</AlertTitle>
      <AlertDescription className="space-y-2">
        <p>{t("monitoring.agent.text")}</p>
        <Command label={t("monitoring.agent.commandLabel")}>{START_AGENT}</Command>
      </AlertDescription>
    </Alert>
  );
}

/** The dashboard: alerts first, then the machine, the services, TAK Server, certificates, the platform, and the history. */
export function OverviewTab({ data }: { data: Monitoring }) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  // After an action the platform looks again at once, so the page shows its effect.
  const reload = async () => {
    try {
      queryClient.setQueryData(["admin", "monitoring"], await api.monitoring(true));
    } catch {
      await queryClient.invalidateQueries({ queryKey: ["admin", "monitoring"] });
    }
  };
  const snapshot = data.snapshot;
  if (!snapshot) {
    return (
      <div className="space-y-6">
        <AlertsCard data={data} />
        <p className="text-sm text-muted-foreground">{t("monitoring.starting")}</p>
      </div>
    );
  }
  return (
    <div className="space-y-6">
      <AgentNotice snapshot={snapshot} />
      <AlertsCard data={data} />
      <HostCard snapshot={snapshot} />
      <ServicesCard snapshot={snapshot} reload={reload} />
      <TakCard snapshot={snapshot} />
      <CertificatesCard snapshot={snapshot} />
      <PlatformCard snapshot={snapshot} reload={reload} />
      {snapshot.agent.status !== "disabled" && <HistoryCharts />}
    </div>
  );
}
