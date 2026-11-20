import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useState, type FormEvent, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { api } from "@/lib/api";
import { useNotifyError } from "@/lib/hooks";
import type { Settings, SettingsView, Thresholds } from "@/lib/monitoring-types";

import { QueryState } from "./shared";

/** A number with its label, its unit and its default. */
function NumberField({ id, label, hint, value, min, max, onChange }: { id: string; label: string; hint?: ReactNode; value: number; min: number; max: number; onChange: (value: number) => void }) {
  return (
    <div className="space-y-1.5">
      <Label htmlFor={id}>{label}</Label>
      <Input id={id} type="number" min={min} max={max} value={Number.isFinite(value) ? value : ""} onChange={(e) => onChange(e.target.valueAsNumber)} className="max-w-28" />
      {hint && <p className="text-xs text-muted-foreground">{hint}</p>}
    </div>
  );
}

function Toggle({ id, label, hint, checked, onChange }: { id: string; label: string; hint?: ReactNode; checked: boolean; onChange: (checked: boolean) => void }) {
  return (
    <div className="flex items-start gap-2">
      <input id={id} type="checkbox" className="mt-1 size-4 accent-primary" checked={checked} onChange={(e) => onChange(e.target.checked)} />
      <div className="space-y-0.5">
        <Label htmlFor={id}>{label}</Label>
        {hint && <p className="text-xs text-muted-foreground">{hint}</p>}
      </div>
    </div>
  );
}

/** How tak.gov writes a version; shown as the expected form. */
const TAK_VERSION_EXAMPLE = "5.8-RELEASE-103";

const THRESHOLDS: { key: keyof Thresholds; min: number; max: number }[] = [
  { key: "disk_percent", min: 50, max: 99 },
  { key: "memory_percent", min: 50, max: 99 },
  { key: "certificate_warning_days", min: 1, max: 365 },
  { key: "certificate_critical_days", min: 1, max: 90 },
  { key: "backup_max_age_hours", min: 25, max: 720 },
  { key: "mail_failures_per_hour", min: 1, max: 1000 },
  { key: "restart_loop_count", min: 2, max: 100 },
  { key: "reconcile_stale_minutes", min: 3, max: 1440 },
];

function SettingsForm({ view }: { view: SettingsView }) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const notifyError = useNotifyError();
  const [draft, setDraft] = useState<Settings>(view.settings);
  const [passphrase, setPassphrase] = useState("");
  const [saving, setSaving] = useState(false);
  const [testing, setTesting] = useState(false);
  const [testingMail, setTestingMail] = useState(false);
  const set = <K extends keyof Settings>(key: K, value: Settings[K]) => setDraft((current) => ({ ...current, [key]: value }));
  const defaults = view.defaults;

  async function store(changes: Parameters<typeof api.updateSettings>[0], done: string) {
    setSaving(true);
    try {
      const saved = await api.updateSettings(changes);
      queryClient.setQueryData(["admin", "settings"], saved);
      setDraft(saved.settings);
      setPassphrase("");
      toast.success(done);
      // The platform looks again with the new settings, so the other tabs show their effect.
      queryClient.setQueryData(["admin", "monitoring"], await api.monitoring(true));
    } catch (e) {
      notifyError(e);
    } finally {
      setSaving(false);
    }
  }

  function save(event: FormEvent) {
    event.preventDefault();
    void store({ settings: draft, ...(passphrase ? { backup_passphrase: passphrase } : {}) }, t("settings.saved"));
  }

  async function testWebhook() {
    setTesting(true);
    try {
      await api.testWebhook();
      toast.success(t("settings.alerts.webhookTested"));
    } catch (e) {
      notifyError(e);
    } finally {
      setTesting(false);
    }
  }

  async function testMail() {
    setTestingMail(true);
    try {
      const result = await api.testMail();
      if (result.sent) {
        toast.success(t("settings.alerts.mailTested", { email: result.to }));
      } else {
        // The mail server's own words, technical and in English: shown as they are.
        toast.error(t("settings.alerts.mailTestFailed"), {
          description: result.error && (
            <span className="font-mono text-xs break-words" translate="no">
              {result.error}
            </span>
          ),
          duration: 20_000,
        });
      }
    } catch (e) {
      notifyError(e);
    } finally {
      setTestingMail(false);
    }
  }

  const webhookSaved = view.settings.webhook_url !== "" && view.settings.webhook_url === draft.webhook_url;
  return (
    <form onSubmit={save} className="space-y-6">
      <Card>
        <CardHeader>
          <CardTitle>{t("settings.alerts.title")}</CardTitle>
          <CardDescription>{t("settings.alerts.description")}</CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          <Toggle id="set-emails" label={t("settings.alerts.emails")} hint={t("settings.alerts.emailsHint")} checked={draft.alert_emails} onChange={(v) => set("alert_emails", v)} />
          <div className="space-y-1.5">
            <Button type="button" variant="outline" disabled={testingMail} onClick={() => void testMail()}>
              {testingMail ? t("common.working") : t("settings.alerts.mailTest")}
            </Button>
            <p className="text-xs text-muted-foreground">{t("settings.alerts.mailTestHint")}</p>
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="set-webhook">{t("settings.alerts.webhook")}</Label>
            <div className="flex flex-wrap gap-2">
              <Input id="set-webhook" type="url" inputMode="url" maxLength={500} value={draft.webhook_url} onChange={(e) => set("webhook_url", e.target.value)} className="max-w-xl flex-1" />
              <Button type="button" variant="outline" disabled={!webhookSaved || testing} onClick={() => void testWebhook()}>
                {testing ? t("common.working") : t("settings.alerts.webhookTest")}
              </Button>
            </div>
            <p className="text-xs text-muted-foreground">{t("settings.alerts.webhookHint")}</p>
          </div>
          <NumberField
            id="set-reminder"
            label={t("settings.alerts.reminder")}
            hint={t("settings.default", { value: defaults.reminder_hours })}
            value={draft.reminder_hours}
            min={1}
            max={336}
            onChange={(v) => set("reminder_hours", v)}
          />
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>{t("settings.thresholds.title")}</CardTitle>
          <CardDescription>{t("settings.thresholds.description")}</CardDescription>
        </CardHeader>
        <CardContent className="grid gap-4 sm:grid-cols-2">
          {THRESHOLDS.map(({ key, min, max }) => (
            <NumberField
              key={key}
              id={`set-${key}`}
              label={t(`settings.thresholds.${key}`)}
              hint={t("settings.default", { value: defaults.thresholds[key] })}
              value={draft.thresholds[key]}
              min={min}
              max={max}
              onChange={(v) => set("thresholds", { ...draft.thresholds, [key]: v })}
            />
          ))}
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>{t("settings.backups.title")}</CardTitle>
          <CardDescription>{t("settings.backups.description")}</CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          <Toggle id="set-backup" label={t("settings.backups.enabled")} hint={t("settings.backups.enabledHint")} checked={draft.backup_enabled} onChange={(v) => set("backup_enabled", v)} />
          <div className="grid gap-4 sm:grid-cols-2">
            <NumberField id="set-hour" label={t("settings.backups.hour")} hint={t("settings.backups.hourHint")} value={draft.backup_hour_utc} min={0} max={23} onChange={(v) => set("backup_hour_utc", v)} />
            <NumberField id="set-keep" label={t("settings.backups.keep")} hint={t("settings.default", { value: defaults.backup_keep })} value={draft.backup_keep} min={1} max={365} onChange={(v) => set("backup_keep", v)} />
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="set-passphrase">{t("settings.backups.passphrase")}</Label>
            <div className="flex flex-wrap gap-2">
              <Input
                id="set-passphrase"
                type="password"
                autoComplete="new-password"
                minLength={12}
                maxLength={200}
                value={passphrase}
                placeholder={view.backup_passphrase_set ? t("settings.backups.passphraseSet") : t("settings.backups.passphraseNone")}
                onChange={(e) => setPassphrase(e.target.value)}
                className="max-w-sm flex-1"
              />
              {view.backup_passphrase_set && (
                <Button type="button" variant="outline" disabled={saving} onClick={() => void store({ backup_passphrase: "" }, t("settings.backups.passphraseRemoved"))}>
                  {t("settings.backups.passphraseRemove")}
                </Button>
              )}
            </div>
            <p className="text-xs text-muted-foreground">{t("settings.backups.passphraseHint")}</p>
          </div>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>{t("settings.other.title")}</CardTitle>
        </CardHeader>
        <CardContent className="grid gap-4 sm:grid-cols-2">
          <NumberField
            id="set-audit"
            label={t("settings.other.auditDays")}
            hint={t("settings.default", { value: defaults.audit_retention_days })}
            value={draft.audit_retention_days}
            min={30}
            max={3650}
            onChange={(v) => set("audit_retention_days", v)}
          />
          <NumberField id="set-logs" label={t("settings.other.logDays")} hint={t("settings.default", { value: defaults.log_keep_days })} value={draft.log_keep_days} min={1} max={365} onChange={(v) => set("log_keep_days", v)} />
          <div className="space-y-1.5 sm:col-span-2">
            <Label htmlFor="set-tak">{t("settings.other.takVersion")}</Label>
            <Input id="set-tak" value={draft.tak_latest_known_version} placeholder={TAK_VERSION_EXAMPLE} onChange={(e) => set("tak_latest_known_version", e.target.value)} className="max-w-xs" translate="no" />
            <p className="text-xs text-muted-foreground">{t("settings.other.takVersionHint")}</p>
          </div>
        </CardContent>
      </Card>

      <Button type="submit" disabled={saving}>
        {saving ? t("common.working") : t("common.save")}
      </Button>
    </form>
  );
}

/** Thresholds, where alerts go, the backup schedule, retention. */
export function SettingsTab() {
  const query = useQuery({ queryKey: ["admin", "settings"], queryFn: api.settings });
  if (!query.data) return <QueryState pending={query.isPending} error={query.error} />;
  // Remounted when the server's values change, so the form starts from them.
  return <SettingsForm key={JSON.stringify(query.data.settings)} view={query.data} />;
}
