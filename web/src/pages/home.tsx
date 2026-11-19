import { useQueryClient } from "@tanstack/react-query";
import { CalendarClockIcon, ChevronRightIcon, MailOpenIcon, PlusIcon, UsersIcon } from "lucide-react";
import { useState, type FormEvent } from "react";
import { useTranslation } from "react-i18next";
import { Link, useNavigate } from "react-router-dom";
import { toast } from "sonner";

import { PageHeader } from "@/components/app-shell";
import { ModeBadge, RoleBadge, StatusBadge } from "@/components/badges";
import { ModePicker } from "@/components/mode-picker";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import { api } from "@/lib/api";
import { useNotifyError } from "@/lib/hooks";
import type { AccessKind, Me, MyInvitation } from "@/lib/types";

function RequestTeamDialog({ open, onOpenChange }: { open: boolean; onOpenChange: (open: boolean) => void }) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const notifyError = useNotifyError();
  const [name, setName] = useState("");
  const [mode, setMode] = useState<AccessKind>("individual");
  const [message, setMessage] = useState("");
  const [pending, setPending] = useState(false);

  async function submit(event: FormEvent) {
    event.preventDefault();
    setPending(true);
    try {
      const team = await api.requestTeam(name, mode, message);
      await queryClient.invalidateQueries({ queryKey: ["me"] });
      toast.success(t("home.request.sent"));
      onOpenChange(false);
      setName("");
      setMessage("");
      navigate(`/teams/${team.id}`);
    } catch (e) {
      notifyError(e);
    } finally {
      setPending(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-2xl" closeLabel={t("common.close")}>
        <form onSubmit={(e) => void submit(e)} className="grid gap-4">
          <DialogHeader>
            <DialogTitle>{t("home.request.title")}</DialogTitle>
            <DialogDescription>{t("home.request.description")}</DialogDescription>
          </DialogHeader>
          <div className="space-y-2">
            <Label htmlFor="team-name">{t("home.request.name")}</Label>
            <Input id="team-name" required minLength={2} maxLength={60} value={name} onChange={(e) => setName(e.target.value)} autoFocus />
            <p className="text-xs text-muted-foreground">{t("home.request.nameHelp")}</p>
          </div>
          <div className="space-y-2">
            <Label>{t("mode.label")}</Label>
            <ModePicker value={mode} onChange={setMode} />
            <p className="text-xs text-muted-foreground">{t("home.request.modeHelp")}</p>
          </div>
          <div className="space-y-2">
            <Label htmlFor="team-message">
              {t("home.request.message")} <span className="font-normal text-muted-foreground">({t("common.optional")})</span>
            </Label>
            <Textarea id="team-message" maxLength={500} rows={3} value={message} onChange={(e) => setMessage(e.target.value)} />
          </div>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)}>
              {t("common.cancel")}
            </Button>
            <Button type="submit" disabled={pending || name.trim().length < 2}>
              {pending ? t("common.working") : t("home.request.submit")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function InvitationCard({ invitation }: { invitation: MyInvitation }) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const notifyError = useNotifyError();
  const [pending, setPending] = useState(false);

  async function answer(accept: boolean) {
    setPending(true);
    try {
      const result = await api.answerInvitation(invitation.id, accept);
      await queryClient.invalidateQueries({ queryKey: ["me"] });
      if (result.accepted) {
        toast.success(t("home.invitations.joined", { team: invitation.team_name }));
        navigate(`/teams/${result.team_id}`);
      }
    } catch (e) {
      notifyError(e);
    } finally {
      setPending(false);
    }
  }

  return (
    <Card className="border-primary/40">
      <CardContent className="flex flex-wrap items-center gap-3">
        <MailOpenIcon className="size-5 text-primary" />
        <p className="flex-1 text-sm">{t("home.invitations.text", { inviter: invitation.invited_by, team: invitation.team_name })}</p>
        <div className="flex gap-2">
          <Button variant="outline" size="sm" disabled={pending} onClick={() => void answer(false)}>
            {t("home.invitations.decline")}
          </Button>
          <Button size="sm" disabled={pending} onClick={() => void answer(true)}>
            {t("home.invitations.accept")}
          </Button>
        </div>
      </CardContent>
    </Card>
  );
}

/** Home: the caller's teams, the invitations waiting for them, and the way to ask for a team. */
export function HomePage({ me }: { me: Me }) {
  const { t } = useTranslation();
  const [requestOpen, setRequestOpen] = useState(false);
  const hasNothing = me.teams.length === 0 && me.invitations.length === 0;

  return (
    <>
      <PageHeader
        title={t("home.title")}
        description={t("home.description")}
        actions={
          <Button onClick={() => setRequestOpen(true)}>
            <PlusIcon />
            {t("home.request.open")}
          </Button>
        }
      />

      {me.invitations.length > 0 && (
        <section className="mb-6 space-y-2" aria-label={t("home.invitations.title")}>
          {me.invitations.map((invitation) => (
            <InvitationCard key={invitation.id} invitation={invitation} />
          ))}
        </section>
      )}

      {hasNothing ? (
        <Card>
          <CardHeader>
            <CardTitle>{t("home.empty.title")}</CardTitle>
            <CardDescription>{t("home.empty.description")}</CardDescription>
          </CardHeader>
          <CardContent>
            <ul className="list-disc space-y-1 pl-5 text-sm text-muted-foreground">
              <li>{t("home.empty.invited")}</li>
              <li>{t("home.empty.request")}</li>
            </ul>
          </CardContent>
        </Card>
      ) : (
        <ul className="grid gap-3 sm:grid-cols-2">
          {me.teams.map((team) => (
            <li key={team.id}>
              <Link to={`/teams/${team.id}`} className="group block rounded-xl outline-none focus-visible:ring-3 focus-visible:ring-ring/50">
                <Card className="transition-colors group-hover:border-primary/50">
                  <CardHeader>
                    <CardTitle className="flex items-center gap-2">
                      <UsersIcon className="size-4 text-primary" />
                      <span className="flex-1 truncate">{team.name}</span>
                      <ChevronRightIcon className="size-4 text-muted-foreground" />
                    </CardTitle>
                    <CardDescription className="flex flex-wrap gap-1.5">
                      <StatusBadge status={team.status} />
                      <RoleBadge role={team.role} />
                      {team.status === "active" && <ModeBadge mode={team.mode} />}
                      {team.event_invitations > 0 && (
                        <Badge className="border-primary/40 text-primary" variant="outline">
                          <CalendarClockIcon />
                          {t("home.eventInvitations", { count: team.event_invitations })}
                        </Badge>
                      )}
                    </CardDescription>
                  </CardHeader>
                </Card>
              </Link>
            </li>
          ))}
        </ul>
      )}

      <RequestTeamDialog open={requestOpen} onOpenChange={setRequestOpen} />
    </>
  );
}
