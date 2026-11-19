import { useQueryClient } from "@tanstack/react-query";
import { ArchiveIcon, LockIcon, TriangleAlertIcon } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { ConfirmDialog } from "@/components/confirm-dialog";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { api } from "@/lib/api";
import { formatBytes, formatDateTime, formatNumber, formatRelative } from "@/lib/format";
import { useNotifyError } from "@/lib/hooks";
import type { Monitoring } from "@/lib/monitoring-types";

import { Command, Field } from "./shared";

const FOLDER = "deploy/backups/";
const RESTORE = "sudo deploy/scripts/restore.sh deploy/backups/<archive>";

/** The hour of the daily backup, given in UTC by the server, shown in the browser's time zone. */
function localHour(hourUtc: number, language: string): string {
  const at = new Date();
  at.setUTCHours(hourUtc, 0, 0, 0);
  return new Intl.DateTimeFormat(language, { hour: "numeric", minute: "2-digit", timeZoneName: "short" }).format(at);
}

/** Backups: what exists, what ran, a button to run one now, and how to restore. */
export function BackupsTab({ data }: { data: Monitoring }) {
  const { t, i18n } = useTranslation();
  const queryClient = useQueryClient();
  const notifyError = useNotifyError();
  const [confirming, setConfirming] = useState(false);
  const snapshot = data.snapshot;
  if (!snapshot) return <p className="text-sm text-muted-foreground">{t("monitoring.starting")}</p>;
  const backups = snapshot.backups;
  const agentOk = snapshot.agent.status === "ok";
  const daily = backups.daily_report;

  async function run() {
    try {
      const { run } = await api.runBackup();
      if (run.ok) toast.success(t("backups.done", { file: run.file ?? "" }));
      else toast.error(t("backups.failed"));
    } catch (e) {
      notifyError(e);
    } finally {
      try {
        queryClient.setQueryData(["admin", "monitoring"], await api.monitoring(true));
      } catch {
        await queryClient.invalidateQueries({ queryKey: ["admin", "monitoring"] });
      }
    }
  }

  return (
    <div className="space-y-6">
      {!agentOk && (
        <Alert variant="destructive">
          <TriangleAlertIcon />
          <AlertTitle>{t("backups.noAgentTitle")}</AlertTitle>
          <AlertDescription>{t("backups.noAgentText")}</AlertDescription>
        </Alert>
      )}
      {agentOk && !backups.passphrase_set && (
        <Alert>
          <TriangleAlertIcon />
          <AlertTitle>{t("backups.notEncryptedTitle")}</AlertTitle>
          <AlertDescription>{t("backups.notEncryptedText")}</AlertDescription>
        </Alert>
      )}
      <Card>
        <CardHeader>
          <CardTitle className="flex flex-wrap items-center justify-between gap-2">
            {t("backups.title")}
            <Button disabled={!agentOk || backups.running} onClick={() => setConfirming(true)}>
              <ArchiveIcon />
              {backups.running ? t("backups.running") : t("backups.runNow")}
            </Button>
          </CardTitle>
          <CardDescription>{t("backups.description")}</CardDescription>
        </CardHeader>
        <CardContent className="space-y-4 text-sm">
          <dl className="grid gap-x-8 sm:grid-cols-2">
            <Field label={t("backups.schedule")}>
              {backups.enabled ? t("backups.scheduleDaily", { hour: localHour(backups.hour_utc, i18n.language) }) : <span className="text-destructive">{t("backups.scheduleOff")}</span>}
            </Field>
            <Field label={t("backups.kept")}>{t("backups.keptValue", { count: backups.keep })}</Field>
            <Field label={t("backups.newest")}>{backups.newest_at ? formatRelative(backups.newest_at) : t("backups.never")}</Field>
            <Field label={t("backups.encryption")}>{backups.passphrase_set ? t("backups.encrypted") : t("backups.notEncrypted")}</Field>
            <Field label={t("backups.folder")}>
              <code className="font-mono text-xs" translate="no">
                {FOLDER}
              </code>
              {agentOk && <span className="text-muted-foreground"> · {formatBytes(backups.total_bytes)}</span>}
            </Field>
            <Field label={t("backups.dailyJobs")}>{backups.daily_ran_at ? formatRelative(backups.daily_ran_at) : t("backups.never")}</Field>
          </dl>
          {backups.error && (
            <p className="font-mono text-xs text-destructive" lang="en">
              {backups.error}
            </p>
          )}
          {backups.daily_ran_at && (
            <p className="text-xs text-muted-foreground">
              {daily.logs && daily.logs.error === undefined && t("backups.dailyLogs", { count: (daily.logs.deleted_files ?? 0) + (daily.logs.rotated_files ?? 0), size: formatBytes(daily.logs.remaining_bytes ?? 0) })}{" "}
              {daily.purged && t("backups.dailyPurged", { count: Object.values(daily.purged).reduce((sum, n) => sum + n, 0) })}{" "}
              {daily.letsencrypt && t("backups.dailyLetsEncrypt", { outcome: daily.letsencrypt.outcome })}
            </p>
          )}

          {backups.files.length === 0 ? (
            <p className="text-muted-foreground">{agentOk ? t("backups.none") : ""}</p>
          ) : (
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>{t("backups.columns.archive")}</TableHead>
                  <TableHead>{t("backups.columns.written")}</TableHead>
                  <TableHead className="text-right">{t("backups.columns.size")}</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {backups.files.map((file) => (
                  <TableRow key={file.name}>
                    <TableCell className="whitespace-normal">
                      <span className="font-mono text-xs break-all" translate="no">
                        {file.name}
                      </span>
                      {file.encrypted && (
                        <Badge variant="outline" className="ml-2">
                          <LockIcon />
                          {t("backups.encryptedBadge")}
                        </Badge>
                      )}
                    </TableCell>
                    <TableCell className="text-muted-foreground">{formatDateTime(file.modified_at)}</TableCell>
                    <TableCell className="text-right tabular-nums">{formatBytes(file.size_bytes)}</TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          )}
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>{t("backups.runsTitle")}</CardTitle>
          <CardDescription>{t("backups.runsHelp")}</CardDescription>
        </CardHeader>
        <CardContent>
          {backups.runs.length === 0 ? (
            <p className="text-sm text-muted-foreground">{t("backups.noRuns")}</p>
          ) : (
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>{t("backups.columns.when")}</TableHead>
                  <TableHead>{t("backups.columns.origin")}</TableHead>
                  <TableHead>{t("backups.columns.result")}</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {backups.runs.map((run) => (
                  <TableRow key={run.id}>
                    <TableCell className="text-muted-foreground">{formatDateTime(run.started_at)}</TableCell>
                    <TableCell>
                      {t(`backups.trigger.${run.trigger}`)}
                      {run.requested_by && <div className="text-xs text-muted-foreground">{run.requested_by}</div>}
                    </TableCell>
                    <TableCell className="whitespace-normal">
                      {run.ok === null ? (
                        <Badge variant="secondary">{t("backups.result.running")}</Badge>
                      ) : run.ok ? (
                        <>
                          <Badge variant="outline">{t("backups.result.ok")}</Badge>
                          {run.size_bytes !== null && <span className="ml-2 text-xs text-muted-foreground">{formatBytes(run.size_bytes)}</span>}
                        </>
                      ) : (
                        <>
                          <Badge variant="destructive">{t("backups.result.failed")}</Badge>
                          {run.error && (
                            <pre className="mt-1 max-w-xl overflow-x-auto font-mono text-xs whitespace-pre-wrap text-destructive" lang="en">
                              {run.error}
                            </pre>
                          )}
                        </>
                      )}
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          )}
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>{t("backups.restoreTitle")}</CardTitle>
          <CardDescription>{t("backups.restoreHelp")}</CardDescription>
        </CardHeader>
        <CardContent className="space-y-3 text-sm">
          <Command label={t("backups.restoreLabel")}>{RESTORE}</Command>
          <ul className="list-disc space-y-1 pl-5 text-muted-foreground">
            <li>{t("backups.restoreNote1")}</li>
            <li>{t("backups.restoreNote2")}</li>
            <li>{t("backups.restoreNote3", { count: formatNumber(backups.max_age_hours) })}</li>
          </ul>
        </CardContent>
      </Card>

      <ConfirmDialog
        open={confirming}
        onOpenChange={setConfirming}
        title={t("backups.confirmTitle")}
        description={<p>{backups.passphrase_set ? t("backups.confirmEncrypted") : t("backups.confirmPlain")}</p>}
        confirmLabel={t("backups.runNow")}
        onConfirm={run}
      />
    </div>
  );
}
