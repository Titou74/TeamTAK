import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ArrowLeftIcon, KeyRoundIcon, MailIcon, PlusIcon, SmartphoneIcon, Trash2Icon, UserMinusIcon, XIcon } from "lucide-react";
import { useState, type FormEvent, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { Link, useNavigate, useParams } from "react-router-dom";
import { toast } from "sonner";

import { AccessDialog } from "@/components/access-dialog";
import { PageHeader } from "@/components/app-shell";
import { ModeBadge, RoleBadge, StatusBadge } from "@/components/badges";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { EventsSection } from "@/components/events-section";
import { ModePicker } from "@/components/mode-picker";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { currentLanguage } from "@/i18n";
import { LANGUAGES } from "@/i18n/languages";
import { api, ApiError } from "@/lib/api";
import { formatDate, formatRelative } from "@/lib/format";
import { useErrorText, useNotifyError } from "@/lib/hooks";
import type { AccessKind, AccessSummary, Me, Member, TeamDetail } from "@/lib/types";

type Reload = () => Promise<unknown>;

function memberName(member: Member): string {
  return member.display_name || member.email;
}

/** One line of the access list: an existing access, or a place where one can be created. */
function AccessRow({
  title,
  subtitle,
  access,
  canCreate,
  onCreate,
  onOpen,
}: {
  title: string;
  subtitle?: string;
  access?: AccessSummary;
  canCreate: boolean;
  onCreate: () => Promise<unknown>;
  onOpen: (access: AccessSummary, title: string) => void;
}) {
  const { t } = useTranslation();
  const [pending, setPending] = useState(false);

  async function create() {
    setPending(true);
    try {
      await onCreate();
    } finally {
      setPending(false);
    }
  }

  return (
    <li className="flex flex-wrap items-center gap-3 py-3">
      <KeyRoundIcon className="size-4 text-muted-foreground" />
      <div className="min-w-0 flex-1">
        <p className="truncate text-sm font-medium">{title}</p>
        <p className="truncate text-xs text-muted-foreground">
          {access ? (
            <>
              <span className="font-mono">{access.username}</span>
              {" · "}
              {access.active_certificates > 0
                ? t("team.access.certificates", {
                    count: access.active_certificates,
                    relative: formatRelative(access.next_expiry),
                  })
                : t("team.access.noCertificate")}
            </>
          ) : (
            (subtitle ?? t("team.access.notCreated"))
          )}
        </p>
      </div>
      {access ? (
        <Button key="open" size="sm" onClick={() => onOpen(access, title)}>
          <SmartphoneIcon />
          {t("team.access.connect")}
        </Button>
      ) : (
        canCreate && (
          <Button key="create" size="sm" variant="outline" disabled={pending} onClick={() => void create()}>
            <PlusIcon />
            {pending ? t("common.working") : t("team.access.create")}
          </Button>
        )
      )}
    </li>
  );
}

function AccessSection({ detail, me, reload }: { detail: TeamDetail; me: Me; reload: Reload }) {
  const { t } = useTranslation();
  const notifyError = useNotifyError();
  const [open, setOpen] = useState<{ id: string; title: string } | null>(null);
  const [label, setLabel] = useState("");
  const [adding, setAdding] = useState(false);
  const { team, accesses, members, permissions } = detail;

  async function create(body: { kind: AccessKind; account_id?: string; label?: string }, title: string) {
    try {
      const created = await api.createAccess(team.id, body);
      await reload();
      if (created.synced) {
        setOpen({ id: created.id, title });
      } else {
        toast.warning(t("team.access.pendingSync"));
      }
    } catch (e) {
      notifyError(e);
    }
  }

  async function addExternal(event: FormEvent) {
    event.preventDefault();
    setAdding(true);
    await create({ kind: "external", label }, t("team.access.externalTitle", { label: label.trim() }));
    setLabel("");
    setAdding(false);
  }

  const openAccess = (access: AccessSummary, title: string) => setOpen({ id: access.id, title });
  const shared = accesses.find((a) => a.kind === "shared");
  const externals = accesses.filter((a) => a.kind === "external");
  // A leader sees every member's access; a member only their own.
  const individualMembers = permissions.create_member_access ? members : members.filter((m) => m.account_id === me.account.id);

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t("team.access.title")}</CardTitle>
        <CardDescription>{t(`mode.${team.mode}.description`)}</CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <ul className="divide-y">
          {team.mode === "shared" && (
            <AccessRow
              title={t("team.access.sharedTitle", { team: team.name })}
              subtitle={permissions.create_shared_access ? undefined : t("team.access.sharedNotReady")}
              access={shared}
              canCreate={permissions.create_shared_access}
              onCreate={() => create({ kind: "shared" }, t("team.access.sharedTitle", { team: team.name }))}
              onOpen={openAccess}
            />
          )}
          {team.mode === "individual" &&
            individualMembers.map((member) => {
              const own = member.account_id === me.account.id;
              const title = own ? t("team.access.mine") : memberName(member);
              return (
                <AccessRow
                  key={member.account_id}
                  title={title}
                  access={accesses.find((a) => a.kind === "individual" && a.account_id === member.account_id)}
                  canCreate={own ? permissions.create_own_access : permissions.create_member_access}
                  onCreate={() => create({ kind: "individual", account_id: member.account_id }, title)}
                  onOpen={openAccess}
                />
              );
            })}
          {externals.map((access) => (
            <AccessRow
              key={access.id}
              title={t("team.access.externalTitle", { label: access.label })}
              access={access}
              canCreate={false}
              onCreate={async () => undefined}
              onOpen={openAccess}
            />
          ))}
        </ul>
        {team.mode === "external" && externals.length === 0 && !permissions.create_external_access && (
          <p className="text-sm text-muted-foreground">{t("team.access.externalOnly")}</p>
        )}
        {permissions.create_external_access && (
          <form onSubmit={(e) => void addExternal(e)} className="flex flex-wrap items-end gap-2 border-t pt-4">
            <div className="min-w-52 flex-1 space-y-1.5">
              <Label htmlFor="external-label">{t("team.access.externalLabel")}</Label>
              <Input
                id="external-label"
                maxLength={40}
                value={label}
                onChange={(e) => setLabel(e.target.value)}
                placeholder={t("team.access.externalPlaceholder")}
              />
            </div>
            <Button type="submit" variant="outline" disabled={adding || label.trim().length === 0}>
              <PlusIcon />
              {t("team.access.externalAdd")}
            </Button>
            <p className="w-full text-xs text-muted-foreground">{t("team.access.externalHelp")}</p>
          </form>
        )}
      </CardContent>
      <AccessDialog accessId={open?.id ?? null} title={open?.title ?? ""} teamId={team.id} onClose={() => setOpen(null)} />
    </Card>
  );
}

function MembersSection({ detail, me, reload }: { detail: TeamDetail; me: Me; reload: Reload }) {
  const { t } = useTranslation();
  const notifyError = useNotifyError();
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const [email, setEmail] = useState("");
  // The inviter's own language unless they pick another one for this invitation.
  const [chosenLanguage, setLanguage] = useState<string | null>(null);
  const language = chosenLanguage ?? currentLanguage();
  const [inviting, setInviting] = useState(false);
  const [removing, setRemoving] = useState<Member | null>(null);
  const { team, members, invitations, permissions, seats_left: seatsLeft } = detail;
  const active = team.status === "active";

  async function invite(event: FormEvent) {
    event.preventDefault();
    setInviting(true);
    try {
      const result = await api.invite(team.id, email, language);
      await reload();
      setEmail("");
      if (result.email_sent) toast.success(t("team.members.invited", { email: result.email }));
      else toast.warning(t("team.members.invitedNoMail", { email: result.email }));
    } catch (e) {
      notifyError(e);
    } finally {
      setInviting(false);
    }
  }

  async function act(action: () => Promise<unknown>) {
    try {
      await action();
      await reload();
    } catch (e) {
      notifyError(e);
    }
  }

  async function remove(member: Member) {
    try {
      await api.removeMember(team.id, member.account_id);
      if (member.account_id === me.account.id) {
        await queryClient.invalidateQueries({ queryKey: ["me"] });
        navigate("/");
      } else {
        await reload();
      }
    } catch (e) {
      notifyError(e);
    }
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t("team.members.title")}</CardTitle>
        <CardDescription>
          {t("team.members.count", { count: members.length, cap: team.member_cap })}
          {permissions.manage_members && active && <> · {t("team.members.seatsLeft", { count: seatsLeft })}</>}
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <ul className="divide-y">
          {members.map((member) => {
            const self = member.account_id === me.account.id;
            return (
              <li key={member.account_id} className="flex flex-wrap items-center gap-3 py-2.5">
                <div className="min-w-0 flex-1">
                  <p className="truncate text-sm font-medium">
                    {memberName(member)}
                    {self && <span className="font-normal text-muted-foreground"> ({t("team.members.you")})</span>}
                  </p>
                  {member.display_name && <p className="truncate text-xs text-muted-foreground">{member.email}</p>}
                </div>
                <RoleBadge role={member.role} />
                {permissions.manage_members && !self && (
                  <Button
                    variant="ghost"
                    size="sm"
                    onClick={() => void act(() => api.setMemberRole(team.id, member.account_id, member.role === "leader" ? "member" : "leader"))}
                  >
                    {member.role === "leader" ? t("team.members.demote") : t("team.members.promote")}
                  </Button>
                )}
                {(permissions.manage_members || self) && (
                  <Button variant="ghost" size="sm" onClick={() => setRemoving(member)}>
                    <UserMinusIcon />
                    {self ? t("team.members.leave") : t("team.members.remove")}
                  </Button>
                )}
              </li>
            );
          })}
        </ul>

        {invitations.length > 0 && (
          <div className="space-y-1.5">
            <p className="text-sm font-medium">{t("team.members.pending")}</p>
            <ul className="divide-y rounded-lg border">
              {invitations.map((invitation) => (
                <li key={invitation.id} className="flex items-center gap-3 px-3 py-2 text-sm">
                  <MailIcon className="size-4 text-muted-foreground" />
                  <span className="min-w-0 flex-1 truncate">{invitation.email}</span>
                  <span className="text-xs text-muted-foreground">{formatDate(invitation.created_at)}</span>
                  {permissions.manage_members && (
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      aria-label={t("team.members.cancelInvitation", { email: invitation.email })}
                      onClick={() => void act(() => api.cancelInvitation(team.id, invitation.id))}
                    >
                      <XIcon />
                    </Button>
                  )}
                </li>
              ))}
            </ul>
          </div>
        )}

        {permissions.manage_members && active && (
          <form onSubmit={(e) => void invite(e)} className="flex flex-wrap items-end gap-2 border-t pt-4">
            <div className="min-w-52 flex-1 space-y-1.5">
              <Label htmlFor="invite-email">{t("team.members.inviteLabel")}</Label>
              <Input
                id="invite-email"
                type="email"
                required
                value={email}
                onChange={(e) => setEmail(e.target.value)}
                placeholder={t("login.emailPlaceholder")}
              />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="invite-language">{t("team.members.inviteLanguage")}</Label>
              <select
                id="invite-language"
                value={language}
                onChange={(e) => setLanguage(e.target.value)}
                className="h-8 rounded-lg border border-input bg-transparent px-2 text-sm outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50"
              >
                {LANGUAGES.map((l) => (
                  <option key={l.code} value={l.code}>
                    {l.label}
                  </option>
                ))}
              </select>
            </div>
            <Button type="submit" disabled={inviting || seatsLeft === 0 || !email}>
              <MailIcon />
              {inviting ? t("common.working") : t("team.members.invite")}
            </Button>
            <p className="w-full text-xs text-muted-foreground">
              {seatsLeft === 0 ? t("team.members.full", { cap: team.member_cap }) : t("team.members.inviteHelp")}
            </p>
          </form>
        )}
      </CardContent>

      <ConfirmDialog
        open={removing !== null}
        onOpenChange={(open) => !open && setRemoving(null)}
        title={removing?.account_id === me.account.id ? t("team.members.leaveTitle") : t("team.members.removeTitle")}
        description={
          <p>
            {removing?.account_id === me.account.id
              ? t("team.members.leaveText", { team: team.name })
              : t("team.members.removeText", { name: removing ? memberName(removing) : "" })}
          </p>
        }
        confirmLabel={removing?.account_id === me.account.id ? t("team.members.leave") : t("team.members.remove")}
        destructive
        onConfirm={() => (removing ? remove(removing) : Promise.resolve())}
      />
    </Card>
  );
}

function SettingsSection({ detail, reload }: { detail: TeamDetail; reload: Reload }) {
  const { t } = useTranslation();
  const notifyError = useNotifyError();
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const [pendingMode, setPendingMode] = useState<AccessKind | null>(null);
  const [deleting, setDeleting] = useState(false);
  const { team, accesses, my_role: myRole, permissions } = detail;
  const isLeader = myRole === "leader";

  const lost = pendingMode === null ? 0 : accesses.filter((a) => a.kind !== "external" && a.kind !== pendingMode).length;

  async function changeMode() {
    if (!pendingMode) return;
    try {
      const result = await api.setTeamMode(team.id, pendingMode);
      await reload();
      toast.success(t("team.settings.modeChanged", { count: result.removed_accesses }));
    } catch (e) {
      notifyError(e);
    }
  }

  async function deleteTeam() {
    try {
      await api.deleteTeam(team.id);
      await queryClient.invalidateQueries({ queryKey: ["me"] });
      toast.success(t("team.settings.deleted", { team: team.name }));
      navigate("/");
    } catch (e) {
      notifyError(e);
    }
  }

  if (!permissions.manage_team) return null;
  return (
    <Card>
      <CardHeader>
        <CardTitle>{t("team.settings.title")}</CardTitle>
      </CardHeader>
      <CardContent className="space-y-6">
        {isLeader && team.status !== "rejected" && (
          <div className="space-y-2">
            <Label>{t("mode.label")}</Label>
            <ModePicker value={team.mode} onChange={(mode) => mode !== team.mode && setPendingMode(mode)} />
            <p className="text-xs text-muted-foreground">{t("team.settings.modeHelp")}</p>
          </div>
        )}
        <div className="flex flex-wrap items-center justify-between gap-3 rounded-lg border border-destructive/30 p-3">
          <div className="space-y-0.5">
            <p className="text-sm font-medium">{t("team.settings.deleteTitle")}</p>
            <p className="text-xs text-muted-foreground">{t("team.settings.deleteHelp")}</p>
          </div>
          <Button variant="destructive" onClick={() => setDeleting(true)}>
            <Trash2Icon />
            {t("team.settings.delete")}
          </Button>
        </div>
      </CardContent>

      <ConfirmDialog
        open={pendingMode !== null}
        onOpenChange={(open) => !open && setPendingMode(null)}
        title={t("team.settings.modeTitle", { mode: pendingMode ? t(`mode.${pendingMode}.name`) : "" })}
        description={
          <>
            <p>{pendingMode ? t(`mode.${pendingMode}.description`) : ""}</p>
            {lost > 0 && <p className="font-medium text-destructive">{t("team.settings.modeLoss", { count: lost })}</p>}
          </>
        }
        confirmLabel={t("team.settings.modeConfirm")}
        destructive={lost > 0}
        onConfirm={changeMode}
      />
      <ConfirmDialog
        open={deleting}
        onOpenChange={setDeleting}
        title={t("team.settings.deleteConfirmTitle", { team: team.name })}
        description={<p>{t("team.settings.deleteConfirmText")}</p>}
        confirmLabel={t("team.settings.delete")}
        destructive
        onConfirm={deleteTeam}
      />
    </Card>
  );
}

function StatusNotice({ detail }: { detail: TeamDetail }) {
  const { t } = useTranslation();
  const { team } = detail;
  if (team.status === "active") return null;
  const note: ReactNode = team.decision_note ? <p className="mt-1">{t("team.notice.reason", { note: team.decision_note })}</p> : null;
  return (
    <Alert variant={team.status === "rejected" ? "destructive" : "default"}>
      <AlertTitle>{t(`team.notice.${team.status}.title`)}</AlertTitle>
      <AlertDescription>
        {t(`team.notice.${team.status}.text`)}
        {team.status !== "pending" && note}
      </AlertDescription>
    </Alert>
  );
}

/** A team: its TAK accesses, its events, its members, its settings. */
export function TeamPage({ me }: { me: Me }) {
  const { t } = useTranslation();
  const { id = "" } = useParams();
  const queryClient = useQueryClient();
  const errorText = useErrorText();
  const query = useQuery({ queryKey: ["team", id], queryFn: () => api.team(id), retry: false });
  const reload: Reload = () =>
    Promise.all([queryClient.invalidateQueries({ queryKey: ["team", id] }), queryClient.invalidateQueries({ queryKey: ["me"] })]);

  const back = (
    <Link to="/" className="mb-4 inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground">
      <ArrowLeftIcon className="size-4" />
      {t("nav.teams")}
    </Link>
  );
  if (query.isPending) {
    return <p className="text-sm text-muted-foreground">{t("common.loading")}</p>;
  }
  if (query.isError) {
    const missing = query.error instanceof ApiError && query.error.status === 404;
    return (
      <>
        {back}
        <Alert variant="destructive">
          <AlertDescription>{missing ? t("team.notFound") : errorText(query.error)}</AlertDescription>
        </Alert>
      </>
    );
  }

  const detail = query.data;
  const { team } = detail;
  return (
    <>
      {back}
      <PageHeader
        title={team.name}
        description={
          <span className="flex flex-wrap items-center gap-1.5">
            <StatusBadge status={team.status} />
            {detail.my_role && <RoleBadge role={detail.my_role} />}
            <ModeBadge mode={team.mode} />
            <span>{t("team.channel", { slug: team.slug })}</span>
          </span>
        }
      />
      <div className="space-y-6">
        <StatusNotice detail={detail} />
        {team.status === "active" && detail.my_role && <AccessSection detail={detail} me={me} reload={reload} />}
        {(team.status === "active" || team.status === "suspended") && <EventsSection team={team} isMember={detail.my_role !== null} />}
        <MembersSection detail={detail} me={me} reload={reload} />
        <SettingsSection detail={detail} reload={reload} />
      </div>
    </>
  );
}
