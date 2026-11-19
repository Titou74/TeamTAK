import { useQuery, useQueryClient } from "@tanstack/react-query";
import { CalendarClockIcon, CalendarPlusIcon, ChevronDownIcon, ChevronRightIcon, LogOutIcon, MailIcon, SquareIcon, XIcon } from "lucide-react";
import { useEffect, useState, type FormEvent } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { EventPhaseBadge } from "@/components/badges";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardAction, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { api, type Synced } from "@/lib/api";
import { browserTimeZone, formatDateTimeRange, formatRelative, fromLocalInput, toLocalInput } from "@/lib/format";
import { currentLanguage } from "@/i18n";
import { useErrorText, useNotifyError } from "@/lib/hooks";
import type { EventTeam, InvitableTeam, Team, TeamEvent } from "@/lib/types";

type Reload = () => Promise<unknown>;

/** The date that says where an event stands: its start, its end, or when it was called off. */
function turningPoint(event: TeamEvent): string {
  if (event.phase === "scheduled") return event.starts_at;
  if (event.phase === "cancelled") return event.cancelled_at ?? event.ends_at;
  return event.ends_at;
}

/** Start and end of an event, entered in the browser's time zone. */
function DateFields({
  start,
  end,
  onStart,
  onEnd,
  startDisabled,
  idPrefix,
}: {
  start: string;
  end: string;
  onStart: (value: string) => void;
  onEnd: (value: string) => void;
  startDisabled?: boolean;
  idPrefix: string;
}) {
  const { t } = useTranslation();
  return (
    <>
      <div className="grid gap-3 sm:grid-cols-2">
        <div className="space-y-1.5">
          <Label htmlFor={`${idPrefix}-start`}>{t("events.form.start")}</Label>
          <Input
            id={`${idPrefix}-start`}
            type="datetime-local"
            required={!startDisabled}
            disabled={startDisabled}
            value={start}
            onChange={(e) => onStart(e.target.value)}
          />
        </div>
        <div className="space-y-1.5">
          <Label htmlFor={`${idPrefix}-end`}>{t("events.form.end")}</Label>
          <Input id={`${idPrefix}-end`} type="datetime-local" required min={start || undefined} value={end} onChange={(e) => onEnd(e.target.value)} />
        </div>
      </div>
      <p className="text-xs text-muted-foreground">{t("events.form.timeZone", { zone: browserTimeZone() })}</p>
    </>
  );
}

function nextHour(offsetHours: number): string {
  const date = new Date();
  date.setMinutes(0, 0, 0);
  date.setHours(date.getHours() + 1 + offsetHours);
  return toLocalInput(date);
}

function CreateEventDialog({ team, open, onOpenChange, reload }: { team: Team; open: boolean; onOpenChange: (open: boolean) => void; reload: Reload }) {
  const { t } = useTranslation();
  const notifyError = useNotifyError();
  const [name, setName] = useState("");
  const [startNow, setStartNow] = useState(false);
  const [start, setStart] = useState(() => nextHour(0));
  const [end, setEnd] = useState(() => nextHour(4));
  const [pending, setPending] = useState(false);

  const startsAt = startNow ? null : fromLocalInput(start);
  const endsAt = fromLocalInput(end);
  // Only the order of the two dates is checked here; the server refuses an end already past.
  const ordered = endsAt !== null && (startsAt === null || new Date(endsAt).getTime() > new Date(startsAt).getTime());
  const valid = name.trim().length >= 2 && (startNow || startsAt !== null) && ordered;

  async function submit(event: FormEvent) {
    event.preventDefault();
    if (!endsAt) return;
    setPending(true);
    try {
      const created = await api.createEvent(team.id, { name, ends_at: endsAt, ...(startsAt ? { starts_at: startsAt } : {}) });
      await reload();
      if (created.synced) toast.success(t("events.create.created"));
      else toast.warning(t("events.pendingSync"));
      onOpenChange(false);
      setName("");
    } catch (e) {
      notifyError(e);
    } finally {
      setPending(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-xl" closeLabel={t("common.close")}>
        <form onSubmit={(e) => void submit(e)} className="grid gap-4">
          <DialogHeader>
            <DialogTitle>{t("events.create.title")}</DialogTitle>
            <DialogDescription>{t("events.create.description", { team: team.name })}</DialogDescription>
          </DialogHeader>
          <div className="space-y-1.5">
            <Label htmlFor="event-name">{t("events.create.name")}</Label>
            <Input id="event-name" required minLength={2} maxLength={60} value={name} onChange={(e) => setName(e.target.value)} autoFocus />
            <p className="text-xs text-muted-foreground">{t("events.create.nameHelp")}</p>
          </div>
          <label className="flex items-center gap-2 text-sm">
            <input type="checkbox" className="size-4 accent-primary" checked={startNow} onChange={(e) => setStartNow(e.target.checked)} />
            {t("events.create.startNow")}
          </label>
          <DateFields idPrefix="event-new" start={startNow ? "" : start} end={end} onStart={setStart} onEnd={setEnd} startDisabled={startNow} />
          {!ordered && endsAt !== null && <p className="text-sm text-destructive">{t("events.form.endBeforeStart")}</p>}
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)}>
              {t("common.cancel")}
            </Button>
            <Button type="submit" disabled={pending || !valid}>
              {pending ? t("common.working") : t("events.create.submit")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function ChangeDatesDialog({ teamId, event, onClose, reload }: { teamId: string; event: TeamEvent; onClose: () => void; reload: Reload }) {
  const { t } = useTranslation();
  const notifyError = useNotifyError();
  const running = event.phase === "running";
  const [start, setStart] = useState(() => toLocalInput(new Date(event.starts_at)));
  const [end, setEnd] = useState(() => toLocalInput(new Date(event.ends_at)));
  const [pending, setPending] = useState(false);

  const startsAt = fromLocalInput(start);
  const endsAt = fromLocalInput(end);
  const ordered = startsAt !== null && endsAt !== null && new Date(endsAt).getTime() > new Date(startsAt).getTime();

  async function submit(submitted: FormEvent) {
    submitted.preventDefault();
    if (!startsAt || !endsAt) return;
    setPending(true);
    try {
      // Once the event runs its start is history: only the end is sent.
      const result = await api.updateEvent(teamId, event.id, running ? { ends_at: endsAt } : { starts_at: startsAt, ends_at: endsAt });
      await reload();
      if (result.synced) toast.success(t("events.dates.done"));
      else toast.warning(t("events.pendingSync"));
      onClose();
    } catch (e) {
      notifyError(e);
    } finally {
      setPending(false);
    }
  }

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="sm:max-w-xl" closeLabel={t("common.close")}>
        <form onSubmit={(e) => void submit(e)} className="grid gap-4">
          <DialogHeader>
            <DialogTitle>{t("events.dates.title", { event: event.name })}</DialogTitle>
            <DialogDescription>{running ? t("events.dates.running") : t("events.dates.description")}</DialogDescription>
          </DialogHeader>
          <DateFields idPrefix="event-edit" start={start} end={end} onStart={setStart} onEnd={setEnd} startDisabled={running} />
          {!ordered && <p className="text-sm text-destructive">{t("events.form.endBeforeStart")}</p>}
          <DialogFooter>
            <Button type="button" variant="outline" onClick={onClose}>
              {t("common.cancel")}
            </Button>
            <Button type="submit" disabled={pending || !ordered}>
              {pending ? t("common.working") : t("events.dates.submit")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

/** The teams of an event. Those who run it also see who has not answered, and can take a team out. */
export function EventTeams({ teams, onRemove }: { teams: EventTeam[]; onRemove?: (team: EventTeam) => void }) {
  const { t } = useTranslation();
  return (
    <ul className="flex flex-wrap gap-1.5">
      {teams.map((team) => {
        const takesPart = team.status === "host" || team.status === "accepted";
        const removable = onRemove && team.team_id && (team.status === "accepted" || team.status === "invited");
        return (
          <li key={`${team.name}-${team.status}`}>
            <Badge variant={takesPart ? "secondary" : "outline"} className={takesPart ? "" : "text-muted-foreground"}>
              <span className="font-medium">{team.name}</span>
              <span className="font-normal">{t(`events.teamStatus.${team.status}`)}</span>
              {removable && (
                <button
                  type="button"
                  className="-mr-1 rounded-full p-0.5 hover:bg-foreground/10"
                  aria-label={t("events.removeTeam.action", { team: team.name })}
                  onClick={() => onRemove(team)}
                >
                  <XIcon className="size-3" />
                </button>
              )}
            </Badge>
          </li>
        );
      })}
    </ul>
  );
}

type Confirmation = { kind: "end" | "cancel" | "leave" } | { kind: "remove"; team: EventTeam };

function EventItem({
  team,
  event,
  invitable,
  isMember,
  reload,
}: {
  team: Team;
  event: TeamEvent;
  invitable: InvitableTeam[];
  isMember: boolean;
  reload: Reload;
}) {
  const { t } = useTranslation();
  const notifyError = useNotifyError();
  const [confirming, setConfirming] = useState<Confirmation | null>(null);
  const [editing, setEditing] = useState(false);
  const [chosen, setChosen] = useState("");
  const [inviting, setInviting] = useState(false);
  const { permissions } = event;

  // A team already invited or taking part cannot be invited again; one that declined, left
  // or was removed can.
  const busy = new Set(event.teams.filter((x) => x.status === "invited" || x.status === "accepted").map((x) => x.team_id));
  const candidates = invitable.filter((x) => !busy.has(x.id)).sort((a, b) => a.name.localeCompare(b.name, currentLanguage()));

  async function run(action: () => Promise<Synced>, done: string) {
    try {
      const result = await action();
      await reload();
      if (result.synced) toast.success(done);
      else toast.warning(t("events.pendingSync"));
    } catch (e) {
      notifyError(e);
      await reload();
    }
  }

  async function invite(submitted: FormEvent) {
    submitted.preventDefault();
    if (!chosen) return;
    setInviting(true);
    try {
      const invited = await api.inviteTeamToEvent(team.id, event.id, chosen);
      await reload();
      setChosen("");
      toast.success(t("events.invite.done", { team: invited.name }));
    } catch (e) {
      notifyError(e);
    } finally {
      setInviting(false);
    }
  }

  const confirmations = {
    end: { title: t("events.end.title", { event: event.name }), text: t("events.end.text"), label: t("events.end.confirm") },
    cancel: { title: t("events.cancel.title", { event: event.name }), text: t("events.cancel.text"), label: t("events.cancel.confirm") },
    leave: { title: t("events.leave.title", { event: event.name }), text: t("events.leave.text"), label: t("events.leave.confirm") },
    remove: {
      title: t("events.removeTeam.title", { team: confirming?.kind === "remove" ? confirming.team.name : "" }),
      text: t("events.removeTeam.text"),
      label: t("events.removeTeam.confirm"),
    },
  };

  function confirmed(): Promise<unknown> {
    if (!confirming) return Promise.resolve();
    switch (confirming.kind) {
      case "end":
        return run(() => api.endEvent(team.id, event.id), t("events.end.done"));
      case "cancel":
        return run(() => api.cancelEvent(team.id, event.id), t("events.cancel.done"));
      case "leave":
        return run(() => api.leaveEvent(team.id, event.id), t("events.leave.done"));
      case "remove": {
        const removed = confirming.team;
        if (!removed.team_id) return Promise.resolve();
        const id = removed.team_id;
        return run(() => api.removeTeamFromEvent(team.id, event.id, id), t("events.removeTeam.done", { team: removed.name }));
      }
    }
  }

  const open = event.phase === "scheduled" || event.phase === "running";
  return (
    <li className="space-y-2 py-4 first:pt-0 last:pb-0">
      <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
        <CalendarClockIcon className="size-4 text-muted-foreground" />
        <span className="text-sm font-medium">{event.name}</span>
        <EventPhaseBadge phase={event.phase} />
        <span className="text-xs text-muted-foreground">
          {event.relation === "host" && isMember ? t("events.hostedByUs") : t("events.hostedBy", { team: event.host_team_name })}
        </span>
      </div>
      <p className="text-sm">
        {formatDateTimeRange(event.starts_at, event.ends_at)}
        <span className="text-muted-foreground">
          {" · "}
          {t(`events.when.${event.phase}`, { relative: formatRelative(turningPoint(event)) })}
        </span>
      </p>
      {event.phase === "cancelled" && event.cancel_reason && (
        <p className="text-xs text-muted-foreground">{t(`events.cancelReason.${event.cancel_reason}`)}</p>
      )}
      {open && (
        <p className="text-xs text-muted-foreground">
          {t("events.channel")} <span className="font-mono text-foreground">{event.group_name}</span>
        </p>
      )}
      <EventTeams teams={event.teams} onRemove={permissions.manage ? (removed) => setConfirming({ kind: "remove", team: removed }) : undefined} />

      {permissions.manage && (
        <form onSubmit={(e) => void invite(e)} className="flex flex-wrap items-end gap-2 pt-1">
          <div className="min-w-52 flex-1 space-y-1.5">
            <Label htmlFor={`invite-${event.id}`}>{t("events.invite.label")}</Label>
            <select
              id={`invite-${event.id}`}
              value={chosen}
              onChange={(e) => setChosen(e.target.value)}
              disabled={candidates.length === 0}
              className="h-8 w-full rounded-lg border border-input bg-transparent px-2 text-sm outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50 disabled:opacity-50"
            >
              <option value="">{candidates.length === 0 ? t("events.invite.noTeam") : t("events.invite.placeholder")}</option>
              {candidates.map((candidate) => (
                <option key={candidate.id} value={candidate.id}>
                  {candidate.name}
                </option>
              ))}
            </select>
          </div>
          <Button type="submit" variant="outline" disabled={inviting || !chosen}>
            <MailIcon />
            {inviting ? t("common.working") : t("events.invite.submit")}
          </Button>
        </form>
      )}

      {(permissions.manage || permissions.cancel || permissions.leave) && (
        <div className="flex flex-wrap gap-1 pt-1">
          {permissions.manage && (
            <Button variant="ghost" size="sm" onClick={() => setEditing(true)}>
              <CalendarClockIcon />
              {t("events.dates.action")}
            </Button>
          )}
          {permissions.end && (
            <Button variant="ghost" size="sm" onClick={() => setConfirming({ kind: "end" })}>
              <SquareIcon />
              {t("events.end.action")}
            </Button>
          )}
          {permissions.cancel && !permissions.end && (
            <Button variant="ghost" size="sm" onClick={() => setConfirming({ kind: "cancel" })}>
              <XIcon />
              {t("events.cancel.action")}
            </Button>
          )}
          {permissions.leave && (
            <Button variant="ghost" size="sm" onClick={() => setConfirming({ kind: "leave" })}>
              <LogOutIcon />
              {t("events.leave.action")}
            </Button>
          )}
        </div>
      )}

      {editing && <ChangeDatesDialog teamId={team.id} event={event} onClose={() => setEditing(false)} reload={reload} />}
      <ConfirmDialog
        open={confirming !== null}
        onOpenChange={(next) => !next && setConfirming(null)}
        title={confirming ? confirmations[confirming.kind].title : ""}
        description={<p>{confirming ? confirmations[confirming.kind].text : ""}</p>}
        confirmLabel={confirming ? confirmations[confirming.kind].label : ""}
        // "Cancel" next to "Cancel the event" would read as the same thing.
        dismissLabel={confirming?.kind === "cancel" ? t("events.cancel.keep") : undefined}
        destructive
        onConfirm={confirmed}
      />
    </li>
  );
}

/** An invitation waiting for the team's answer; only its leaders see it. */
function InvitationItem({ team, event, reload }: { team: Team; event: TeamEvent; reload: Reload }) {
  const { t } = useTranslation();
  const notifyError = useNotifyError();
  const [pending, setPending] = useState(false);
  const others = event.teams.filter((x) => x.status !== "host").map((x) => x.name);

  async function answer(accept: boolean) {
    setPending(true);
    try {
      const result = await api.answerEvent(team.id, event.id, accept);
      await reload();
      if (!result.synced) toast.warning(t("events.pendingSync"));
      else if (accept) toast.success(t("events.invitation.accepted", { event: event.name }));
      else toast.success(t("events.invitation.declined"));
    } catch (e) {
      notifyError(e);
      await reload();
    } finally {
      setPending(false);
    }
  }

  return (
    <li className="space-y-2 rounded-lg border border-primary/40 p-3">
      <div className="flex flex-wrap items-start gap-3">
        <MailIcon className="mt-0.5 size-4 text-primary" />
        <div className="min-w-0 flex-1 space-y-1">
          <p className="text-sm font-medium">{t("events.invitation.text", { host: event.host_team_name, event: event.name })}</p>
          <p className="text-sm">
            {formatDateTimeRange(event.starts_at, event.ends_at)}
            <span className="text-muted-foreground">
              {" · "}
              {t(`events.when.${event.phase}`, { relative: formatRelative(turningPoint(event)) })}
            </span>
          </p>
          <p className="text-xs text-muted-foreground">
            {others.length > 0 ? t("events.invitation.others", { teams: others.join(", ") }) : t("events.invitation.first")}{" "}
            {t("events.invitation.effect")}
          </p>
        </div>
        <div className="flex gap-2">
          <Button variant="outline" size="sm" disabled={pending} onClick={() => void answer(false)}>
            {t("events.invitation.decline")}
          </Button>
          <Button size="sm" disabled={pending || team.status !== "active"} onClick={() => void answer(true)}>
            {t("events.invitation.accept")}
          </Button>
        </div>
      </div>
    </li>
  );
}

/**
 * The events of a team: the ones it hosts, takes part in, or is invited to. `isMember` is
 * false for an administrator looking at a team they do not belong to.
 */
export function EventsSection({ team, isMember }: { team: Team; isMember: boolean }) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const errorText = useErrorText();
  const [creating, setCreating] = useState(false);
  const [showPast, setShowPast] = useState(false);
  const query = useQuery({ queryKey: ["team-events", team.id], queryFn: () => api.teamEvents(team.id), refetchInterval: 30_000 });
  const events = query.data?.events;
  const manages = events?.some((event) => event.permissions.manage) ?? false;
  const invitable = useQuery({
    queryKey: ["invitable-teams", team.id],
    queryFn: () => api.invitableTeams(team.id),
    enabled: manages && team.status === "active",
  });
  const reload: Reload = () =>
    Promise.all([
      queryClient.invalidateQueries({ queryKey: ["team-events", team.id] }),
      queryClient.invalidateQueries({ queryKey: ["invitable-teams", team.id] }),
      queryClient.invalidateQueries({ queryKey: ["me"] }),
    ]);

  // An event changes state at its start and at its end without anyone clicking: look again
  // right after the next of those moments.
  const nextChange = events
    ?.filter((event) => event.phase === "scheduled" || event.phase === "running")
    .map((event) => new Date(turningPoint(event)).getTime())
    .sort((a, b) => a - b)[0];
  useEffect(() => {
    if (nextChange === undefined) return;
    const wait = nextChange - Date.now() + 1500;
    // Longer waits are covered by the periodic refresh (and overflow a timer).
    if (wait > 24 * 3600 * 1000) return;
    const timer = window.setTimeout(() => void queryClient.invalidateQueries({ queryKey: ["team-events", team.id] }), Math.max(wait, 1000));
    return () => window.clearTimeout(timer);
  }, [nextChange, queryClient, team.id]);

  // "Starts in 5 minutes" is computed when the list is drawn: draw it again now and then, so
  // it does not go stale between two answers of the server.
  const [, setTick] = useState(0);
  useEffect(() => {
    const timer = window.setInterval(() => setTick((tick) => tick + 1), 30_000);
    return () => window.clearInterval(timer);
  }, []);

  const invitations = events?.filter((event) => event.relation === "invited") ?? [];
  const current = events?.filter((event) => event.relation !== "invited" && (event.phase === "scheduled" || event.phase === "running")) ?? [];
  const past = events?.filter((event) => event.phase === "ended" || event.phase === "cancelled") ?? [];
  const canCreate = query.data?.permissions.create ?? false;

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t("events.title")}</CardTitle>
        <CardDescription>{isMember ? t("events.description") : t("events.descriptionOutside")}</CardDescription>
        {canCreate && (
          <CardAction>
            <Button variant="outline" onClick={() => setCreating(true)}>
              <CalendarPlusIcon />
              {t("events.create.open")}
            </Button>
          </CardAction>
        )}
      </CardHeader>
      <CardContent className="space-y-4">
        {query.isPending && <p className="text-sm text-muted-foreground">{t("common.loading")}</p>}
        {query.isError && (
          <Alert variant="destructive">
            <AlertDescription>{errorText(query.error)}</AlertDescription>
          </Alert>
        )}
        {events && (
          <>
            {invitations.length > 0 && (
              <ul className="space-y-2" aria-label={t("events.invitation.title")}>
                {invitations.map((event) => (
                  <InvitationItem key={event.id} team={team} event={event} reload={reload} />
                ))}
              </ul>
            )}
            {current.length > 0 ? (
              <>
                <ul className="divide-y">
                  {current.map((event) => (
                    <EventItem
                      key={event.id}
                      team={team}
                      event={event}
                      invitable={invitable.data?.teams ?? []}
                      isMember={isMember}
                      reload={reload}
                    />
                  ))}
                </ul>
                {team.mode === "shared" && isMember && <p className="text-xs text-muted-foreground">{t("events.sharedNote")}</p>}
              </>
            ) : (
              invitations.length === 0 && (
                <p className="text-sm text-muted-foreground">
                  {t("events.none")} {canCreate ? t("events.noneLeader") : t("events.noneMember")}
                </p>
              )
            )}
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
                {showPast && (
                  <ul className="mt-3 divide-y">
                    {past.map((event) => (
                      <EventItem key={event.id} team={team} event={event} invitable={[]} isMember={isMember} reload={reload} />
                    ))}
                  </ul>
                )}
              </div>
            )}
          </>
        )}
      </CardContent>
      <CreateEventDialog team={team} open={creating} onOpenChange={setCreating} reload={reload} />
    </Card>
  );
}
