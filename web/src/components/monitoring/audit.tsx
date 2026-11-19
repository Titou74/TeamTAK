import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { useState, type FormEvent } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router-dom";

import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { api } from "@/lib/api";
import { formatBytes, formatDateTime, formatNumber } from "@/lib/format";
import type { AuditEntry, AuditFilter } from "@/lib/monitoring-types";

import { QueryState } from "./shared";

const selectClass = "h-8 w-full rounded-lg border border-input bg-transparent px-2 text-sm outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50";

/** The start of a day typed in a date field, in the browser's time zone, as an instant. */
function dayStart(value: string, addDays = 0): string | undefined {
  if (!value) return undefined;
  const date = new Date(`${value}T00:00:00`);
  if (Number.isNaN(date.getTime())) return undefined;
  date.setDate(date.getDate() + addDays);
  return date.toISOString();
}

/** The details of an entry: named values the server recorded, never free text. */
function Details({ entry }: { entry: AuditEntry }) {
  const { t, i18n } = useTranslation();
  const items = Object.entries(entry.details).map(([key, value]) => {
    const label = i18n.exists(`audit.details.${key}`) ? t(`audit.details.${key}`) : key;
    let shown: string;
    if (typeof value === "boolean") shown = value ? t("audit.yes") : t("audit.no");
    else if (key === "size_bytes" && typeof value === "number") shown = formatBytes(value);
    else if ((key === "starts_at" || key === "ends_at") && typeof value === "string") shown = formatDateTime(value);
    else if (typeof value === "number") shown = formatNumber(value);
    else {
      // Coded values (a mode, a role, an outcome) have a name; the rest is shown as recorded.
      const code = `${key}_${value}`;
      shown = i18n.exists(`audit.values.${code}`) ? t(`audit.values.${code}`) : value;
    }
    return { key, label, shown };
  });
  if (items.length === 0) return null;
  return (
    <ul className="text-xs text-muted-foreground">
      {items.map((item) => (
        <li key={item.key}>
          {item.label} <span className="text-foreground">{item.shown}</span>
        </li>
      ))}
    </ul>
  );
}

/** Who did what, when, on which team or object, from which address. */
export function AuditTab() {
  const { t, i18n } = useTranslation();
  const [draft, setDraft] = useState({ from: "", to: "", team_id: "", actor: "", action: "" });
  const [filter, setFilter] = useState<AuditFilter>({ page: 1 });
  const query = useQuery({ queryKey: ["admin", "audit", filter], queryFn: () => api.audit(filter), placeholderData: keepPreviousData });

  function apply(event: FormEvent) {
    event.preventDefault();
    setFilter({
      from: dayStart(draft.from),
      // The end day is included: up to the start of the next one.
      to: dayStart(draft.to, 1),
      team_id: draft.team_id || undefined,
      actor: draft.actor.trim() || undefined,
      action: draft.action || undefined,
      page: 1,
    });
  }

  function reset() {
    setDraft({ from: "", to: "", team_id: "", actor: "", action: "" });
    setFilter({ page: 1 });
  }

  if (!query.data) return <QueryState pending={query.isPending} error={query.error} />;
  const data = query.data;
  const pages = Math.max(1, Math.ceil(data.total / data.page_size));
  const actionLabel = (code: string) => (i18n.exists(`audit.actions.${code}`) ? t(`audit.actions.${code}`) : code);
  const actions = [...data.actions].sort((a, b) => actionLabel(a).localeCompare(actionLabel(b), i18n.language));
  const who = (entry: AuditEntry) => entry.actor_email || (entry.ip === "command-line" ? t("audit.commandLine") : t("audit.system"));
  return (
    <Card>
      <CardHeader>
        <CardTitle>{t("audit.title")}</CardTitle>
        <CardDescription>{t("audit.description", { count: data.retention_days })}</CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <form onSubmit={apply} className="grid items-end gap-3 sm:grid-cols-2 lg:grid-cols-5">
          <div className="space-y-1.5">
            <Label htmlFor="audit-from">{t("audit.filter.from")}</Label>
            <Input id="audit-from" type="date" value={draft.from} max={draft.to || undefined} onChange={(e) => setDraft({ ...draft, from: e.target.value })} />
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="audit-to">{t("audit.filter.to")}</Label>
            <Input id="audit-to" type="date" value={draft.to} min={draft.from || undefined} onChange={(e) => setDraft({ ...draft, to: e.target.value })} />
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="audit-team">{t("audit.filter.team")}</Label>
            <select id="audit-team" className={selectClass} value={draft.team_id} onChange={(e) => setDraft({ ...draft, team_id: e.target.value })}>
              <option value="">{t("audit.filter.anyTeam")}</option>
              {data.teams.map((team) => (
                <option key={team.id} value={team.id}>
                  {team.name}
                </option>
              ))}
            </select>
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="audit-actor">{t("audit.filter.actor")}</Label>
            <Input id="audit-actor" value={draft.actor} placeholder={t("audit.filter.actorHint")} onChange={(e) => setDraft({ ...draft, actor: e.target.value })} />
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="audit-action">{t("audit.filter.action")}</Label>
            <select id="audit-action" className={selectClass} value={draft.action} onChange={(e) => setDraft({ ...draft, action: e.target.value })}>
              <option value="">{t("audit.filter.anyAction")}</option>
              {actions.map((code) => (
                <option key={code} value={code}>
                  {actionLabel(code)}
                </option>
              ))}
            </select>
          </div>
          <div className="flex gap-2 sm:col-span-2 lg:col-span-5">
            <Button type="submit">{t("audit.filter.apply")}</Button>
            <Button type="button" variant="ghost" onClick={reset}>
              {t("audit.filter.reset")}
            </Button>
          </div>
        </form>

        {data.entries.length === 0 ? (
          <p className="text-sm text-muted-foreground">{t("audit.none")}</p>
        ) : (
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>{t("audit.columns.when")}</TableHead>
                <TableHead>{t("audit.columns.who")}</TableHead>
                <TableHead>{t("audit.columns.what")}</TableHead>
                <TableHead>{t("audit.columns.on")}</TableHead>
                <TableHead>{t("audit.columns.address")}</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {data.entries.map((entry) => (
                <TableRow key={entry.id}>
                  <TableCell className="text-muted-foreground">{formatDateTime(entry.occurred_at)}</TableCell>
                  <TableCell className={entry.actor_email ? "" : "text-muted-foreground"}>{who(entry)}</TableCell>
                  <TableCell className="whitespace-normal">
                    {actionLabel(entry.action)}
                    <Details entry={entry} />
                  </TableCell>
                  <TableCell className="whitespace-normal">
                    {entry.team_id && entry.team_name && (
                      <Link to={`/teams/${entry.team_id}`} className="hover:underline">
                        {entry.team_name}
                      </Link>
                    )}
                    {entry.object && (
                      <div className="font-mono text-xs break-words text-muted-foreground" translate="no">
                        {entry.object}
                      </div>
                    )}
                  </TableCell>
                  <TableCell className="font-mono text-xs text-muted-foreground" translate="no">
                    {entry.ip === "command-line" ? "" : entry.ip}
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        )}

        <div className="flex flex-wrap items-center justify-between gap-2 text-sm text-muted-foreground">
          <span>{t("audit.total", { count: data.total })}</span>
          <span className="flex items-center gap-2">
            <Button variant="outline" size="sm" disabled={data.page <= 1} onClick={() => setFilter({ ...filter, page: data.page - 1 })}>
              {t("audit.previous")}
            </Button>
            {t("audit.page", { page: formatNumber(data.page), pages: formatNumber(pages) })}
            <Button variant="outline" size="sm" disabled={data.page >= pages} onClick={() => setFilter({ ...filter, page: data.page + 1 })}>
              {t("audit.next")}
            </Button>
          </span>
        </div>
      </CardContent>
    </Card>
  );
}
