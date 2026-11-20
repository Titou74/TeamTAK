import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  CalendarPlusIcon,
  CheckIcon,
  ChevronDownIcon,
  ChevronRightIcon,
  CircleAlertIcon,
  CircleCheckIcon,
  PauseIcon,
  PlayIcon,
  RefreshCwIcon,
  Trash2Icon,
  XIcon,
} from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router-dom";
import { toast } from "sonner";

import { PageHeader } from "@/components/app-shell";
import { ModeBadge, StatusBadge } from "@/components/badges";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { adminEventActions, CreateEventDialog, EventItem, useEventClock, type Reload } from "@/components/events-section";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardAction, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { Textarea } from "@/components/ui/textarea";
import { api } from "@/lib/api";
import { formatDate, formatDateTime, formatNumber, formatRelative } from "@/lib/format";
import { useErrorText, useNotifyError } from "@/lib/hooks";
import type { AdminTeam, AdminTeams, Me, TeamEvent } from "@/lib/types";

function RequestCard({ team, defaults, reload }: { team: AdminTeam; defaults: AdminTeams; reload: () => Promise<unknown> }) {
  const { t } = useTranslation();
  const notifyError = useNotifyError();
  const [cap, setCap] = useState(String(team.member_cap));
  const [note, setNote] = useState("");
  const [pending, setPending] = useState(false);

  async function decide(approve: boolean) {
    setPending(true);
    try {
      await api.decideTeam(team.id, approve, approve ? { member_cap: Number(cap), note } : { note });
      await reload();
      toast.success(approve ? t("admin.requests.approved", { team: team.name }) : t("admin.requests.rejected", { team: team.name }));
    } catch (e) {
      notifyError(e);
    } finally {
      setPending(false);
    }
  }

  const capValid = Number.isInteger(Number(cap)) && Number(cap) >= 1 && Number(cap) <= defaults.max_member_cap;
  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex flex-wrap items-center gap-2">
          {team.name}
          <ModeBadge mode={team.mode} />
        </CardTitle>
        <CardDescription>
          {t("admin.requests.by", {
            requester: team.requester_name ? `${team.requester_name} (${team.requester_email})` : team.requester_email,
            date: formatDate(team.created_at),
          })}
          {" · "}
          {t("team.channel", { slug: team.slug })}
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-3">
        {team.request_message && <blockquote className="border-l-2 pl-3 text-sm text-muted-foreground">{team.request_message}</blockquote>}
        <div className="grid gap-3 sm:grid-cols-[8rem_1fr]">
          <div className="space-y-1.5">
            <Label htmlFor={`cap-${team.id}`}>{t("admin.memberCap")}</Label>
            <Input
              id={`cap-${team.id}`}
              type="number"
              min={1}
              max={defaults.max_member_cap}
              value={cap}
              onChange={(e) => setCap(e.target.value)}
              aria-invalid={!capValid}
            />
          </div>
          <div className="space-y-1.5">
            <Label htmlFor={`note-${team.id}`}>
              {t("admin.requests.note")} <span className="font-normal text-muted-foreground">({t("common.optional")})</span>
            </Label>
            <Textarea id={`note-${team.id}`} rows={1} maxLength={500} value={note} onChange={(e) => setNote(e.target.value)} />
          </div>
        </div>
        <div className="flex justify-end gap-2">
          <Button variant="outline" disabled={pending} onClick={() => void decide(false)}>
            <XIcon />
            {t("admin.requests.reject")}
          </Button>
          <Button disabled={pending || !capValid} onClick={() => void decide(true)}>
            <CheckIcon />
            {t("admin.requests.approve")}
          </Button>
        </div>
      </CardContent>
    </Card>
  );
}

function TeamRow({ team, defaults, reload }: { team: AdminTeam; defaults: AdminTeams; reload: () => Promise<unknown> }) {
  const { t } = useTranslation();
  const notifyError = useNotifyError();
  const [cap, setCap] = useState(String(team.member_cap));
  const [confirm, setConfirm] = useState<"suspend" | "delete" | null>(null);
  const capNumber = Number(cap);
  const capChanged = capNumber !== team.member_cap && Number.isInteger(capNumber) && capNumber >= 1 && capNumber <= defaults.max_member_cap;

  async function run(action: () => Promise<unknown>, done: string) {
    try {
      await action();
      await reload();
      toast.success(done);
    } catch (e) {
      notifyError(e);
    }
  }

  return (
    <TableRow>
      <TableCell>
        <Link to={`/teams/${team.id}`} className="font-medium hover:underline">
          {team.name}
        </Link>
        <div className="text-xs text-muted-foreground">{team.requester_email}</div>
      </TableCell>
      <TableCell>
        <StatusBadge status={team.status} />
      </TableCell>
      <TableCell>
        <ModeBadge mode={team.mode} />
      </TableCell>
      <TableCell className="tabular-nums">
        {formatNumber(team.members)}
        {team.pending_invitations > 0 && (
          <span className="text-muted-foreground"> {t("admin.teams.plusInvited", { count: team.pending_invitations })}</span>
        )}
      </TableCell>
      <TableCell className="tabular-nums">{formatNumber(team.accesses)}</TableCell>
      <TableCell>
        <div className="flex items-center gap-1">
          <Input
            type="number"
            min={1}
            max={defaults.max_member_cap}
            value={cap}
            onChange={(e) => setCap(e.target.value)}
            className="h-7 w-16"
            aria-label={t("admin.memberCapFor", { team: team.name })}
          />
          {capChanged && (
            <Button size="icon-sm" aria-label={t("common.save")} onClick={() => void run(() => api.adminUpdateTeam(team.id, { member_cap: capNumber }), t("admin.teams.capSaved"))}>
              <CheckIcon />
            </Button>
          )}
        </div>
      </TableCell>
      <TableCell className="text-right">
        {team.status === "active" && (
          <Button variant="ghost" size="sm" onClick={() => setConfirm("suspend")}>
            <PauseIcon />
            {t("admin.teams.suspend")}
          </Button>
        )}
        {team.status === "suspended" && (
          <Button variant="ghost" size="sm" onClick={() => void run(() => api.adminUpdateTeam(team.id, { suspended: false }), t("admin.teams.resumed", { team: team.name }))}>
            <PlayIcon />
            {t("admin.teams.resume")}
          </Button>
        )}
        <Button variant="ghost" size="icon-sm" aria-label={t("admin.teams.deleteFor", { team: team.name })} onClick={() => setConfirm("delete")}>
          <Trash2Icon />
        </Button>
        <ConfirmDialog
          open={confirm === "suspend"}
          onOpenChange={(open) => !open && setConfirm(null)}
          title={t("admin.teams.suspendTitle", { team: team.name })}
          description={<p>{t("admin.teams.suspendText")}</p>}
          confirmLabel={t("admin.teams.suspend")}
          destructive
          onConfirm={() => run(() => api.adminUpdateTeam(team.id, { suspended: true }), t("admin.teams.suspended", { team: team.name }))}
        />
        <ConfirmDialog
          open={confirm === "delete"}
          onOpenChange={(open) => !open && setConfirm(null)}
          title={t("team.settings.deleteConfirmTitle", { team: team.name })}
          description={<p>{t("team.settings.deleteConfirmText")}</p>}
          confirmLabel={t("team.settings.delete")}
          destructive
          onConfirm={() => run(() => api.deleteTeam(team.id), t("team.settings.deleted", { team: team.name }))}
        />
      </TableCell>
    </TableRow>
  );
}

function TeamsTab() {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const errorText = useErrorText();
  const query = useQuery({ queryKey: ["admin", "teams"], queryFn: api.adminTeams });
  const reload = () => queryClient.invalidateQueries({ queryKey: ["admin", "teams"] });

  if (query.isPending) return <p className="text-sm text-muted-foreground">{t("common.loading")}</p>;
  if (query.isError) {
    return (
      <Alert variant="destructive">
        <AlertDescription>{errorText(query.error)}</AlertDescription>
      </Alert>
    );
  }
  const pending = query.data.teams.filter((team) => team.status === "pending");
  const others = query.data.teams.filter((team) => team.status !== "pending");
  return (
    <div className="space-y-8">
      <section className="space-y-3">
        <h2 className="text-lg font-semibold">{t("admin.requests.title", { count: pending.length })}</h2>
        {pending.length === 0 ? (
          <p className="text-sm text-muted-foreground">{t("admin.requests.none")}</p>
        ) : (
          pending.map((team) => <RequestCard key={team.id} team={team} defaults={query.data} reload={reload} />)
        )}
      </section>
      <section className="space-y-3">
        <h2 className="text-lg font-semibold">{t("admin.teams.title")}</h2>
        {others.length === 0 ? (
          <p className="text-sm text-muted-foreground">{t("admin.teams.none")}</p>
        ) : (
          <Card>
            <CardContent>
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>{t("admin.teams.team")}</TableHead>
                    <TableHead>{t("admin.teams.status")}</TableHead>
                    <TableHead>{t("admin.teams.mode")}</TableHead>
                    <TableHead>{t("admin.teams.members")}</TableHead>
                    <TableHead>{t("admin.teams.accesses")}</TableHead>
                    <TableHead>{t("admin.memberCap")}</TableHead>
                    <TableHead />
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {others.map((team) => (
                    <TeamRow key={team.id} team={team} defaults={query.data} reload={reload} />
                  ))}
                </TableBody>
              </Table>
            </CardContent>
          </Card>
        )}
      </section>
    </div>
  );
}

function EventsTab() {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const errorText = useErrorText();
  const [creating, setCreating] = useState(false);
  const [showPast, setShowPast] = useState(false);
  const query = useQuery({ queryKey: ["admin", "events"], queryFn: api.adminEvents, refetchInterval: 30_000 });
  // The teams an administrator can put in an event: the active ones.
  const teamsQuery = useQuery({ queryKey: ["admin", "teams"], queryFn: api.adminTeams });
  const activeTeams = (teamsQuery.data?.teams ?? []).filter((team) => team.status === "active").map(({ id, name }) => ({ id, name }));
  const reload: Reload = () =>
    Promise.all([
      queryClient.invalidateQueries({ queryKey: ["admin", "events"] }),
      // The teams put in an event see it on their page.
      queryClient.invalidateQueries({ queryKey: ["team-events"] }),
    ]);
  useEventClock(query.data?.events, () => queryClient.invalidateQueries({ queryKey: ["admin", "events"] }));

  if (query.isPending) return <p className="text-sm text-muted-foreground">{t("common.loading")}</p>;
  if (query.isError) {
    return (
      <Alert variant="destructive">
        <AlertDescription>{errorText(query.error)}</AlertDescription>
      </Alert>
    );
  }
  const events = query.data.events;
  const current = events.filter((event) => event.phase === "scheduled" || event.phase === "running");
  const past = events.filter((event) => event.phase === "ended" || event.phase === "cancelled");
  const item = (event: TeamEvent) => (
    <EventItem key={event.id} event={event} actions={adminEventActions(event.id)} addable={activeTeams} isMember={false} reload={reload} />
  );
  return (
    <Card>
      <CardHeader>
        <CardTitle>{t("admin.events.title", { count: events.length })}</CardTitle>
        <CardDescription>{t("admin.events.description")}</CardDescription>
        <CardAction>
          <Button variant="outline" onClick={() => setCreating(true)}>
            <CalendarPlusIcon />
            {t("events.create.open")}
          </Button>
        </CardAction>
      </CardHeader>
      <CardContent className="space-y-4">
        {current.length === 0 ? <p className="text-sm text-muted-foreground">{t("admin.events.none")}</p> : <ul className="divide-y">{current.map(item)}</ul>}
        {past.length > 0 && (
          <div className="border-t pt-3">
            <button
              type="button"
              className="flex items-center gap-1 text-sm font-medium text-muted-foreground hover:text-foreground"
              aria-expanded={showPast}
              onClick={() => setShowPast(!showPast)}
            >
              {showPast ? <ChevronDownIcon className="size-4" /> : <ChevronRightIcon className="size-4" />}
              {t("events.past", { count: past.length })}
            </button>
            {showPast && <ul className="mt-3 divide-y">{past.map(item)}</ul>}
          </div>
        )}
      </CardContent>
      <CreateEventDialog
        open={creating}
        onOpenChange={setCreating}
        description={t("admin.events.create.description")}
        teams={activeTeams}
        create={api.adminCreateEvent}
        createdText={t("admin.events.create.created")}
        reload={reload}
      />
    </Card>
  );
}

function AccountsTab({ me }: { me: Me }) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const errorText = useErrorText();
  const notifyError = useNotifyError();
  const query = useQuery({ queryKey: ["admin", "accounts"], queryFn: api.adminAccounts });

  async function setAdmin(id: string, isAdmin: boolean) {
    try {
      await api.setAdmin(id, isAdmin);
      await queryClient.invalidateQueries({ queryKey: ["admin", "accounts"] });
    } catch (e) {
      notifyError(e);
    }
  }

  if (query.isPending) return <p className="text-sm text-muted-foreground">{t("common.loading")}</p>;
  if (query.isError) {
    return (
      <Alert variant="destructive">
        <AlertDescription>{errorText(query.error)}</AlertDescription>
      </Alert>
    );
  }
  return (
    <Card>
      <CardHeader>
        <CardTitle>{t("admin.accounts.title", { count: query.data.accounts.length })}</CardTitle>
        <CardDescription>{t("admin.accounts.description")}</CardDescription>
      </CardHeader>
      <CardContent>
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>{t("admin.accounts.account")}</TableHead>
              <TableHead>{t("admin.accounts.teams")}</TableHead>
              <TableHead>{t("admin.accounts.lastLogin")}</TableHead>
              <TableHead />
            </TableRow>
          </TableHeader>
          <TableBody>
            {query.data.accounts.map((account) => (
              <TableRow key={account.id}>
                <TableCell>
                  <span className="font-medium">{account.display_name || account.email}</span>
                  {account.is_admin && (
                    <Badge className="ml-2" variant="secondary">
                      {t("role.admin")}
                    </Badge>
                  )}
                  {account.display_name && <div className="text-xs text-muted-foreground">{account.email}</div>}
                </TableCell>
                <TableCell className="tabular-nums">{formatNumber(account.teams)}</TableCell>
                <TableCell className="text-muted-foreground">
                  {account.last_login_at ? formatDateTime(account.last_login_at) : t("admin.accounts.never")}
                </TableCell>
                <TableCell className="text-right">
                  {account.id !== me.account.id && (
                    <Button variant="ghost" size="sm" onClick={() => void setAdmin(account.id, !account.is_admin)}>
                      {account.is_admin ? t("admin.accounts.revokeAdmin") : t("admin.accounts.makeAdmin")}
                    </Button>
                  )}
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </CardContent>
    </Card>
  );
}

function Reachable({ ok, label }: { ok: boolean; label: string }) {
  const { t } = useTranslation();
  return (
    <div className="flex items-center gap-2 rounded-lg border bg-card p-3 text-sm">
      {ok ? <CircleCheckIcon className="size-4 text-primary" /> : <CircleAlertIcon className="size-4 text-destructive" />}
      <span className="flex-1 font-medium">{label}</span>
      <span className={ok ? "text-muted-foreground" : "text-destructive"}>{ok ? t("admin.sync.reachable") : t("admin.sync.unreachable")}</span>
    </div>
  );
}

function SyncTab() {
  const { t, i18n } = useTranslation();
  const queryClient = useQueryClient();
  const errorText = useErrorText();
  const notifyError = useNotifyError();
  const [running, setRunning] = useState(false);
  const query = useQuery({ queryKey: ["admin", "reconciliation"], queryFn: api.reconciliation, refetchInterval: 30_000 });

  async function run() {
    setRunning(true);
    try {
      queryClient.setQueryData(["admin", "reconciliation"], await api.runReconciliation());
      toast.success(t("admin.sync.ran"));
    } catch (e) {
      notifyError(e);
    } finally {
      setRunning(false);
    }
  }

  if (query.isPending) return <p className="text-sm text-muted-foreground">{t("common.loading")}</p>;
  if (query.isError) {
    return (
      <Alert variant="destructive">
        <AlertDescription>{errorText(query.error)}</AlertDescription>
      </Alert>
    );
  }
  const data = query.data;
  const kindLabel = (kind: string) => (i18n.exists(`reconcile.kinds.${kind}`) ? t(`reconcile.kinds.${kind}`) : kind);
  return (
    <div className="space-y-6">
      <Card>
        <CardHeader>
          <CardTitle>{t("admin.sync.title")}</CardTitle>
          <CardDescription>{t("admin.sync.description", { seconds: data.interval_seconds })}</CardDescription>
        </CardHeader>
        <CardContent className="space-y-3">
          <div className="grid gap-2 sm:grid-cols-2">
            <Reachable ok={data.directory_reachable} label={t("admin.sync.directory")} />
            <Reachable ok={data.tak_reachable} label={t("admin.sync.takServer")} />
          </div>
          <dl className="grid gap-x-6 gap-y-1 text-sm sm:grid-cols-2">
            <div className="flex justify-between gap-2">
              <dt className="text-muted-foreground">{t("admin.sync.lastRun")}</dt>
              <dd>{data.last_run_at ? formatRelative(data.last_run_at) : t("admin.accounts.never")}</dd>
            </div>
            <div className="flex justify-between gap-2">
              <dt className="text-muted-foreground">{t("admin.sync.lastOk")}</dt>
              <dd>{data.last_ok_at ? formatRelative(data.last_ok_at) : t("admin.accounts.never")}</dd>
            </div>
            <div className="flex justify-between gap-2">
              <dt className="text-muted-foreground">{t("admin.sync.expected")}</dt>
              <dd>{t("admin.sync.expectedValue", { accounts: data.expected_accounts, groups: data.expected_groups })}</dd>
            </div>
            <div className="flex justify-between gap-2">
              <dt className="text-muted-foreground">{t("admin.sync.drift7d")}</dt>
              <dd className={data.drift_last_7_days > 0 ? "font-medium text-destructive" : ""}>{formatNumber(data.drift_last_7_days)}</dd>
            </div>
          </dl>
          {data.errors.length > 0 && (
            <Alert variant="destructive">
              <AlertTitle>{t("admin.sync.errorsTitle")}</AlertTitle>
              <AlertDescription>
                {/* Technical messages from the server log, shown as they are (English). */}
                <ul className="list-disc pl-4 font-mono text-xs" lang="en">
                  {data.errors.map((error) => (
                    <li key={error}>{error}</li>
                  ))}
                </ul>
              </AlertDescription>
            </Alert>
          )}
          <Button variant="outline" disabled={running} onClick={() => void run()}>
            <RefreshCwIcon className={running ? "animate-spin" : ""} />
            {running ? t("common.working") : t("admin.sync.run")}
          </Button>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>{t("admin.sync.journal")}</CardTitle>
          <CardDescription>{t("admin.sync.journalHelp")}</CardDescription>
        </CardHeader>
        <CardContent>
          {data.events.length === 0 ? (
            <p className="text-sm text-muted-foreground">{t("admin.sync.noEvents")}</p>
          ) : (
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>{t("admin.sync.when")}</TableHead>
                  <TableHead>{t("admin.sync.what")}</TableHead>
                  <TableHead>{t("admin.sync.account")}</TableHead>
                  <TableHead>{t("admin.sync.group")}</TableHead>
                  <TableHead>{t("admin.sync.origin")}</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {data.events.map((event) => (
                  <TableRow key={event.id}>
                    <TableCell className="text-muted-foreground">{formatDateTime(event.occurred_at)}</TableCell>
                    <TableCell>{kindLabel(event.kind)}</TableCell>
                    <TableCell className="font-mono text-xs">{event.subject}</TableCell>
                    <TableCell className="font-mono text-xs">{event.group_name ?? ""}</TableCell>
                    <TableCell>
                      {event.is_drift ? (
                        <Badge variant="destructive">{t("admin.sync.drift")}</Badge>
                      ) : (
                        <Badge variant="outline">{t(`reconcile.triggers.${event.trigger}`)}</Badge>
                      )}
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          )}
        </CardContent>
      </Card>
    </div>
  );
}

/** Administration: team requests, teams, events, accounts, and the directory's state. */
export function AdminPage({ me }: { me: Me }) {
  const { t } = useTranslation();
  return (
    <>
      <PageHeader title={t("admin.title")} description={t("admin.description")} />
      <Tabs defaultValue="teams">
        <TabsList>
          <TabsTrigger value="teams">{t("admin.tabs.teams")}</TabsTrigger>
          <TabsTrigger value="events">{t("admin.tabs.events")}</TabsTrigger>
          <TabsTrigger value="accounts">{t("admin.tabs.accounts")}</TabsTrigger>
          <TabsTrigger value="sync">{t("admin.tabs.sync")}</TabsTrigger>
        </TabsList>
        <TabsContent value="teams" className="pt-4">
          <TeamsTab />
        </TabsContent>
        <TabsContent value="events" className="pt-4">
          <EventsTab />
        </TabsContent>
        <TabsContent value="accounts" className="pt-4">
          <AccountsTab me={me} />
        </TabsContent>
        <TabsContent value="sync" className="pt-4">
          <SyncTab />
        </TabsContent>
      </Tabs>
    </>
  );
}
