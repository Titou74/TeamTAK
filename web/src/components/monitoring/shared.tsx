import { CircleCheckIcon } from "lucide-react";
import type { ReactNode } from "react";
import { useCallback } from "react";
import { useTranslation } from "react-i18next";

import { CopyButton } from "@/components/copy-button";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { formatNumber } from "@/lib/format";
import { useErrorText } from "@/lib/hooks";
import type { Alert as AlertData, Severity } from "@/lib/monitoring-types";
import { cn } from "@/lib/utils";

export function SeverityBadge({ severity }: { severity: Severity }) {
  const { t } = useTranslation();
  const variant = severity === "critical" ? "destructive" : severity === "warning" ? "default" : "secondary";
  return <Badge variant={variant}>{t(`alerts.severity.${severity}`)}</Badge>;
}

/** Takes the place of the severity on an alert that is over: the problem is gone. */
export function ResolvedBadge() {
  const { t } = useTranslation();
  return (
    <Badge variant="outline" className="border-emerald-600/30 bg-emerald-500/10 text-emerald-700 dark:border-emerald-400/30 dark:text-emerald-400">
      <CircleCheckIcon />
      {t("alerts.resolved")}
    </Badge>
  );
}

/**
 * The sentence of an alert in the current language. Kinds are stable codes of the server
 * (`alerts.kinds.<code>`); a subject that is one of the platform's own codes (a certificate,
 * a filesystem) has a name, the others (a service, an event) are shown as they are.
 */
export function useAlertText() {
  const { t, i18n } = useTranslation();
  return useCallback(
    (alert: Pick<AlertData, "kind" | "subject" | "details">): string => {
      if (!i18n.exists(`alerts.kinds.${alert.kind}`)) return [alert.kind, alert.subject].filter(Boolean).join(" ");
      const subject = alert.subject && i18n.exists(`alerts.subjects.${alert.subject}`) ? t(`alerts.subjects.${alert.subject}`) : alert.subject;
      // Figures are written the way the current language writes numbers.
      const details = Object.fromEntries(Object.entries(alert.details).map(([key, value]) => [key, typeof value === "number" ? formatNumber(value) : value]));
      return t(`alerts.kinds.${alert.kind}`, { ...details, subject });
    },
    [t, i18n],
  );
}

/** A label and its value on one line of a details list. */
export function Field({ label, children, className }: { label: ReactNode; children: ReactNode; className?: string }) {
  return (
    <div className={cn("flex items-baseline justify-between gap-3 py-1", className)}>
      <dt className="shrink-0 text-muted-foreground">{label}</dt>
      <dd className="min-w-0 text-right">{children}</dd>
    </div>
  );
}

/** A command the administrator runs over SSH: the platform shows it and never runs it. */
export function Command({ children, label }: { children: string; label: string }) {
  return (
    <div className="flex items-center gap-2">
      <code className="min-w-0 flex-1 overflow-x-auto rounded-md bg-muted px-2 py-1.5 font-mono text-xs whitespace-nowrap" translate="no">
        {children}
      </code>
      <CopyButton value={children} label={label} />
    </div>
  );
}

/** What a tab shows while its data loads, or when it could not be loaded. */
export function QueryState({ pending, error }: { pending: boolean; error: unknown }) {
  const { t } = useTranslation();
  const errorText = useErrorText();
  if (pending) return <p className="text-sm text-muted-foreground">{t("common.loading")}</p>;
  return (
    <Alert variant="destructive">
      <AlertDescription>{errorText(error)}</AlertDescription>
    </Alert>
  );
}

/** A coloured dot for a state: fine, needs attention, broken, unknown. */
export function StateDot({ state }: { state: "ok" | "warning" | "bad" | "unknown" }) {
  return (
    <span
      aria-hidden
      className={cn(
        "inline-block size-2 shrink-0 rounded-full",
        state === "ok" && "bg-emerald-500",
        state === "warning" && "bg-amber-500",
        state === "bad" && "bg-destructive",
        state === "unknown" && "bg-muted-foreground/40",
      )}
    />
  );
}

/** A horizontal gauge from 0 to 100, coloured from the thresholds that make an alert. */
export function Meter({ percent, warnAt, label }: { percent: number; warnAt: number; label: string }) {
  const level = percent >= Math.min(warnAt + 5, 97) ? "bg-destructive" : percent >= warnAt ? "bg-amber-500" : "bg-primary";
  return (
    <div
      role="meter"
      aria-label={label}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={Math.round(percent)}
      className="h-1.5 w-full overflow-hidden rounded-full bg-muted"
    >
      <div className={cn("h-full rounded-full", level)} style={{ width: `${Math.max(0, Math.min(100, percent))}%` }} />
    </div>
  );
}
